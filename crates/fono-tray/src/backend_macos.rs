// SPDX-License-Identifier: GPL-3.0-only
//! macOS menu-bar backend: renders the shared [`crate::menu`] model as
//! a native `NSStatusItem`. Phase 7 Task 7.3 of
//! `plans/2026-07-03-macos-port-v1.md`.
//!
//! # Architecture: one pump, two threads
//!
//! AppKit is main-thread-only — `NSStatusItem`, `NSMenu`, and friends
//! must be created and mutated on the process's first thread while an
//! `NSApplication` run loop is pumping events (menu tracking is run
//! entirely by that loop). The daemon, however, lives on tokio worker
//! threads. The split:
//!
//! * `fono`'s `main()` (macOS, graphical session, daemon invocation
//!   only) calls [`install_main_pump`] **before** spawning the daemon
//!   thread, then parks the real main thread in [`run_main_pump`] —
//!   an `NSApplication` with the `Accessory` activation policy (no
//!   Dock icon). Jobs travel on libdispatch's **main queue**
//!   (`dispatch_async_f`), which the AppKit run loop drains as they
//!   arrive — delivery is event-driven, not polled, so a menu repaint
//!   or an overlay blit runs within the loop's next turnaround
//!   instead of waiting for a timer tick.
//! * [`spawn`] (called from the daemon like every other backend) keeps
//!   the provider-polling loop on tokio — identical cadence and
//!   diffing to the ksni backend — and, whenever something changed,
//!   builds the platform-neutral `MenuNode` tree *on the tokio side*
//!   (it's pure data) and dispatches one closure to the main thread
//!   that re-renders `NSMenu`/icon/tooltip from it.
//!
//! If the pump was never installed (headless SSH, subcommand
//! invocations, tests), [`spawn`] degrades gracefully: one warn line,
//! `false` return, daemon continues without a tray — same contract as
//! the Linux backend on a session without a StatusNotifierWatcher.
//!
//! # Click model
//!
//! macOS menu-bar convention: a left click opens the menu (there is no
//! separate "activate" gesture like SNI's). `TrayAction::ActivateLeftClick`
//! is therefore never emitted on macOS; the "Show last transcription"
//! menu row covers that intent.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::rc::Retained;
use objc2::{
    define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBitmapImageRep, NSControlStateValueOff,
    NSControlStateValueOn, NSDeviceRGBColorSpace, NSEvent, NSEventType, NSImage, NSMenu,
    NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSObject, NSPoint, NSSize, NSString};
use tokio::sync::mpsc;

use super::menu::{self, MenuInputs, MenuNode};
use super::{
    ActiveBackends, ActiveProvider, DiscoveredSttProvider, GpuUpgradeProvider, LlmEnabledProvider,
    McpEnabledProvider, MicrophonesProvider, PreferencesProvider, PreferencesSnapshot,
    RecentProvider, TrayAction, TrayState, UpdateProvider, WyomingEnabledProvider,
};

// -------------------------------------------------------------------------
// Main-thread pump.
// -------------------------------------------------------------------------

/// A unit of work shipped from the tokio side to the AppKit main
/// thread. The pump proves main-thread-ness by handing the closure a
/// [`MainThreadMarker`].
type Job = Box<dyn FnOnce(MainThreadMarker) + Send>;

/// Opaque token returned by [`install_main_pump`] and consumed by
/// [`run_main_pump`]. Zero-sized: jobs ride libdispatch's main queue,
/// so there is no channel to carry — the token only enforces the
/// install-then-run handshake at the type level.
pub struct MainPumpJobs(());

/// Set by [`install_main_pump`]. Gates [`dispatch`]: without it
/// (headless / non-daemon invocations) jobs are refused so callers
/// can degrade gracefully instead of enqueueing work that never runs.
static PUMP_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Set when [`run_main_pump`] returns (daemon shutdown). Refuses late
/// dispatches whose jobs would otherwise queue up forever.
static PUMP_EXITED: AtomicBool = AtomicBool::new(false);

// libdispatch (GCD) — lives in libSystem, which every macOS binary
// links: no new crates, no extra link flags. The AppKit run loop
// drains the main queue as jobs arrive, so delivery is event-driven —
// the overlay repaints at the producers' cadence (20–30 fps level/FFT
// ticks) instead of the 10 fps a 100 ms polling timer allowed.
#[repr(C)]
struct DispatchQueueS {
    _priv: [u8; 0],
}

extern "C" {
    /// The immortal main-queue object (`dispatch_get_main_queue()` is
    /// a macro that takes this static's address).
    static _dispatch_main_q: DispatchQueueS;
    fn dispatch_async_f(
        queue: *const DispatchQueueS,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
}

/// Trampoline: rebox the [`Job`] shipped through libdispatch and run
/// it. The main queue guarantees main-thread execution; the marker
/// probe is belt-and-braces.
extern "C" fn run_job(context: *mut c_void) {
    // SAFETY: `context` is the `Box::into_raw` of exactly one
    // `Box<Job>` produced by `dispatch`; libdispatch delivers it
    // exactly once.
    let job = unsafe { Box::from_raw(context.cast::<Job>()) };
    if let Some(mtm) = MainThreadMarker::new() {
        job(mtm);
    }
}

/// Mark the pump as installed. Must be called **before** the daemon
/// thread starts (so tray spawn can never race the install) and
/// followed by [`run_main_pump`] on the main thread. Returns `None` if
/// a pump was already installed (double daemon start — caller bails).
/// Jobs dispatched between install and run queue on the main dispatch
/// queue and execute as soon as the run loop starts.
#[must_use]
pub fn install_main_pump() -> Option<MainPumpJobs> {
    if PUMP_INSTALLED.swap(true, Ordering::SeqCst) {
        return None;
    }
    Some(MainPumpJobs(()))
}

/// Ship a closure to the main thread via the main dispatch queue.
/// Returns `false` when no pump is installed (headless / non-daemon
/// invocations) or the pump exited.
fn dispatch(job: impl FnOnce(MainThreadMarker) + Send + 'static) -> bool {
    if !PUMP_INSTALLED.load(Ordering::SeqCst) || PUMP_EXITED.load(Ordering::SeqCst) {
        return false;
    }
    let job: Box<Job> = Box::new(Box::new(job));
    // SAFETY: `_dispatch_main_q` is libdispatch's process-lifetime
    // main-queue object; `run_job` reboxes `context` exactly once.
    unsafe { dispatch_async_f(&raw const _dispatch_main_q, Box::into_raw(job).cast(), run_job) };
    true
}

/// Public dispatch seam for other AppKit consumers in the process
/// (the overlay's NSPanel backend): ship a plain closure to the pump.
/// The closure runs on the main thread with the run loop pumping; it
/// can obtain its own `MainThreadMarker` cheaply via
/// `MainThreadMarker::new()`. Returns `false` when no pump is
/// installed (headless / non-daemon invocations) or the pump exited.
pub fn dispatch_main(job: Box<dyn FnOnce() + Send>) -> bool {
    dispatch(move |_mtm| job())
}

/// Park the calling thread (which MUST be the process main thread) in
/// the AppKit run loop, executing jobs shipped via [`dispatch`] /
/// [`dispatch_main`]. Returns when [`stop_main_pump`] runs.
///
/// The `Accessory` activation policy keeps fono out of the Dock and
/// the Cmd+Tab switcher — it exists only as a menu-bar item, matching
/// every other dictation utility on the platform.
pub fn run_main_pump(_token: MainPumpJobs) {
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::error!("run_main_pump called off the main thread — tray disabled");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    tracing::debug!("tray: AppKit main-thread pump running (GCD main queue)");
    app.run();
    PUMP_EXITED.store(true, Ordering::SeqCst);
    tracing::debug!("tray: AppKit main-thread pump stopped");
}

/// Ask the pump to exit `NSApplication::run`. Callable from any
/// thread; a no-op when no pump is installed. `stop` only takes
/// effect after the run loop processes an event, so a dummy
/// application-defined event is posted to wake it immediately.
pub fn stop_main_pump() {
    dispatch(|mtm| {
        let app = NSApplication::sharedApplication(mtm);
        app.stop(None);
        // Wake the run loop with an app-defined event so `stop` takes
        // effect immediately (all payload fields zero).
        let dummy = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
            NSEventType::ApplicationDefined,
            NSPoint::ZERO,
            objc2_app_kit::NSEventModifierFlags::empty(),
            0.0,
            0,
            None,
            0,
            0,
            0,
        );
        if let Some(dummy) = dummy {
            app.postEvent_atStart(&dummy, true);
        }
    });
}

// -------------------------------------------------------------------------
// Menu-item target/action bridge.
// -------------------------------------------------------------------------

/// Ivars for the click target: the daemon's action channel plus the
/// per-render registry mapping `NSMenuItem` tags back to the
/// [`TrayAction`]s they were rendered from ([`TrayAction`] carries
/// payloads, so a bare tag integer can't encode it directly).
struct TargetIvars {
    actions: mpsc::UnboundedSender<TrayAction>,
    registry: RefCell<Vec<TrayAction>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and
    // FonoTrayTarget does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FonoTrayTarget"]
    #[ivars = TargetIvars]
    struct FonoTrayTarget;

    impl FonoTrayTarget {
        #[unsafe(method(trayAction:))]
        fn tray_action(&self, sender: &NSMenuItem) {
            let idx = usize::try_from(sender.tag()).unwrap_or(usize::MAX);
            let action = self.ivars().registry.borrow().get(idx).copied();
            if let Some(action) = action {
                let _ = self.ivars().actions.send(action);
            }
        }
    }
);

impl FonoTrayTarget {
    fn new(mtm: MainThreadMarker, actions: mpsc::UnboundedSender<TrayAction>) -> Retained<Self> {
        let this =
            Self::alloc(mtm).set_ivars(TargetIvars { actions, registry: RefCell::new(Vec::new()) });
        // SAFETY: plain NSObject init.
        unsafe { msg_send![super(this), init] }
    }
}

// -------------------------------------------------------------------------
// Main-thread-owned UI state.
// -------------------------------------------------------------------------

struct TrayUi {
    status_item: Retained<NSStatusItem>,
    target: Retained<FonoTrayTarget>,
    tooltip: String,
}

thread_local! {
    /// The live status item. Thread-local (only ever touched from the
    /// main thread, via pump jobs) so no locking or `Send` shims are
    /// needed around the non-`Send` AppKit handles.
    static TRAY_UI: RefCell<Option<TrayUi>> = const { RefCell::new(None) };
}

/// Create the status item. Runs on the main thread.
fn init_ui(mtm: MainThreadMarker, tooltip: String, actions: mpsc::UnboundedSender<TrayAction>) {
    let status_item =
        NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    let target = FonoTrayTarget::new(mtm, actions);
    apply_state_to_item(mtm, &status_item, TrayState::Idle, &tooltip);
    TRAY_UI.with(|ui| *ui.borrow_mut() = Some(TrayUi { status_item, target, tooltip }));
    tracing::debug!("tray icon ready (NSStatusItem)");
}

/// Repaint icon + tooltip for a state change. Runs on the main thread.
fn apply_state(mtm: MainThreadMarker, state: TrayState) {
    TRAY_UI.with(|ui| {
        if let Some(ui) = ui.borrow().as_ref() {
            apply_state_to_item(mtm, &ui.status_item, state, &ui.tooltip);
        }
    });
}

fn apply_state_to_item(
    mtm: MainThreadMarker,
    item: &NSStatusItem,
    state: TrayState,
    tooltip: &str,
) {
    if let Some(button) = item.button(mtm) {
        button.setImage(Some(&icon_image(state)));
        let tip = format!("{tooltip}\n{}", menu::status_label(state));
        button.setToolTip(Some(&NSString::from_str(&tip)));
    }
}

/// Re-render the menu from a freshly built node tree. Runs on the
/// main thread.
fn apply_menu(mtm: MainThreadMarker, nodes: &[MenuNode]) {
    TRAY_UI.with(|ui| {
        if let Some(ui) = ui.borrow().as_ref() {
            let mut registry = Vec::new();
            let ns_menu = render_nodes(mtm, nodes, &ui.target, &mut registry);
            *ui.target.ivars().registry.borrow_mut() = registry;
            ui.status_item.setMenu(Some(&ns_menu));
        }
    });
}

/// Interpret the platform-neutral [`MenuNode`] tree into an `NSMenu`.
/// This is the entire macOS renderer: it never changes when the menu
/// content evolves — edit [`crate::menu::build`] instead.
fn render_nodes(
    mtm: MainThreadMarker,
    nodes: &[MenuNode],
    target: &FonoTrayTarget,
    registry: &mut Vec<TrayAction>,
) -> Retained<NSMenu> {
    let ns_menu = NSMenu::new(mtm);
    // We manage enabled/disabled ourselves (AppKit's auto-enabling
    // would disable every item because our target isn't in the
    // responder chain's usual places).
    ns_menu.setAutoenablesItems(false);
    for node in nodes {
        match node {
            MenuNode::Separator => ns_menu.addItem(&NSMenuItem::separatorItem(mtm)),
            MenuNode::Item { label, action } => {
                let item = plain_item(mtm, label);
                match action {
                    Some(action) => wire(&item, target, registry, *action),
                    None => item.setEnabled(false),
                }
                ns_menu.addItem(&item);
            }
            MenuNode::Check { label, checked, action } => {
                let item = plain_item(mtm, label);
                wire(&item, target, registry, *action);
                item.setState(if *checked {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                });
                ns_menu.addItem(&item);
            }
            MenuNode::Menu { label, children } => {
                let item = plain_item(mtm, label);
                let submenu = render_nodes(mtm, children, target, registry);
                item.setSubmenu(Some(&submenu));
                ns_menu.addItem(&item);
            }
        }
    }
    ns_menu
}

fn plain_item(mtm: MainThreadMarker, label: &str) -> Retained<NSMenuItem> {
    // SAFETY: title/keyEquivalent are plain NSStrings; nil action is
    // wired (or the item disabled) right after by the caller.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(label),
            None,
            &NSString::from_str(""),
        )
    }
}

/// Point a menu item's target/action at our bridge object and record
/// its [`TrayAction`] in the registry (tag = registry index).
fn wire(
    item: &NSMenuItem,
    target: &FonoTrayTarget,
    registry: &mut Vec<TrayAction>,
    action: TrayAction,
) {
    let idx = registry.len();
    registry.push(action);
    // SAFETY: target outlives the menu (both live in TRAY_UI; the
    // registry is swapped atomically with the menu on every render).
    unsafe {
        item.setTarget(Some(target.as_ref()));
        item.setAction(Some(sel!(trayAction:)));
    }
    item.setTag(isize::try_from(idx).unwrap_or(0));
}

/// Solid-colour circle icon tinted by FSM state — same rasterizer
/// shape and palette as the Linux backend (`menu::state_color`), in
/// RGBA byte order for `NSBitmapImageRep`. Rendered at 32×32 px and
/// displayed at 18×18 pt, the conventional menu-bar icon size.
fn icon_image(state: TrayState) -> Retained<NSImage> {
    const SIZE: i32 = 32;
    const POINT_SIZE: f64 = 18.0;
    let (r, g, b) = menu::state_color(state);
    let mut data = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let c = SIZE / 2;
    let radius = (SIZE / 2) - 2;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x - c, y - c);
            if dx * dx + dy * dy <= radius * radius {
                data.extend_from_slice(&[r, g, b, 0xff]);
            } else {
                data.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    // SAFETY: NULL planes ask the rep to allocate its own buffer; we
    // then copy exactly pixelsWide*pixelsHigh*4 bytes into it. All
    // geometry arguments are consistent (32-bit RGBA, meshed).
    unsafe {
        let rep = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            SIZE as isize,
            SIZE as isize,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            (SIZE * 4) as isize,
            32,
        )
        .expect("NSBitmapImageRep allocation for a 32x32 RGBA icon cannot fail");
        std::ptr::copy_nonoverlapping(data.as_ptr(), rep.bitmapData(), data.len());
        let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(POINT_SIZE, POINT_SIZE));
        image.addRepresentation(&rep);
        image
    }
}

// -------------------------------------------------------------------------
// Backend entry point (same contract as the Linux backend's `spawn`).
// -------------------------------------------------------------------------

/// Everything the menu is rendered from, owned. One snapshot per poll
/// tick; compared against the previous one so unchanged ticks ship
/// nothing to the main thread.
#[derive(Clone, PartialEq)]
struct Snapshot {
    state: TrayState,
    recent: Vec<String>,
    active: ActiveBackends,
    discovered_stt: Vec<String>,
    update_label: Option<String>,
    gpu_upgrade_label: Option<String>,
    microphones: (Vec<String>, u8),
    prefs: PreferencesSnapshot,
    mcp_server_enabled: bool,
    wyoming_server_enabled: bool,
    llm_server_enabled: bool,
}

/// Spawn the macOS tray. Returns `true` on success — meaning the
/// main-thread pump is installed and the poll task is running; the
/// status item itself materialises asynchronously via the pump.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
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
) -> bool {
    if tokio::runtime::Handle::try_current().is_err() {
        tracing::warn!("tray backend skipped: no current tokio runtime");
        return false;
    }
    if !PUMP_INSTALLED.load(Ordering::SeqCst) {
        // Not launched through the daemon path in a graphical session
        // (`fono::main` only installs the pump there), so there is no
        // main thread to render AppKit on. Same graceful no-tray mode
        // as Linux-without-a-watcher.
        tracing::warn!(
            "tray unavailable: no AppKit main-thread pump installed (headless launch or \
             embedded use). Dictation and hotkeys continue without the menu-bar icon."
        );
        return false;
    }

    // Materialise the status item.
    dispatch(move |mtm| init_ui(mtm, tooltip, actions));

    tokio::spawn(async move {
        let mut snap = Snapshot {
            state: TrayState::Idle,
            recent: Vec::new(),
            active: ActiveBackends::unknown(),
            discovered_stt: Vec::new(),
            update_label: None,
            gpu_upgrade_label: None,
            microphones: (Vec::new(), u8::MAX),
            prefs: PreferencesSnapshot::default(),
            mcp_server_enabled: mcp_enabled_provider(),
            wyoming_server_enabled: wyoming_enabled_provider(),
            llm_server_enabled: llm_enabled_provider(),
        };
        // First render.
        push_menu(&snap, &stt_labels, &polish_labels, &assistant_labels, &tts_labels);

        let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                state = state_rx.recv() => {
                    let Some(state) = state else { break };
                    if state == snap.state {
                        continue;
                    }
                    snap.state = state;
                    dispatch(move |mtm| apply_state(mtm, state));
                    // The menu embeds the status row, so a state flip
                    // re-renders it too.
                    push_menu(&snap, &stt_labels, &polish_labels, &assistant_labels, &tts_labels);
                }
                _ = interval.tick() => {
                    let next = Snapshot {
                        state: snap.state,
                        recent: recent_provider(),
                        active: active_provider(),
                        discovered_stt: discovered_stt_provider(),
                        update_label: update_provider(),
                        gpu_upgrade_label: gpu_upgrade_provider(),
                        microphones: microphones_provider(),
                        prefs: preferences_provider(),
                        mcp_server_enabled: mcp_enabled_provider(),
                        wyoming_server_enabled: wyoming_enabled_provider(),
                        llm_server_enabled: llm_enabled_provider(),
                    };
                    if next == snap {
                        continue;
                    }
                    snap = next;
                    push_menu(&snap, &stt_labels, &polish_labels, &assistant_labels, &tts_labels);
                }
            }
        }
        tracing::debug!("tray poll task exited (daemon shutting down)");
    });
    true
}

/// Build the menu tree from a snapshot (pure, tokio side) and ship it
/// to the main thread for rendering.
fn push_menu(
    snap: &Snapshot,
    stt_labels: &[String],
    polish_labels: &[String],
    assistant_labels: &[String],
    tts_labels: &[String],
) {
    let inputs = MenuInputs {
        state: snap.state,
        recent: &snap.recent,
        stt_labels,
        polish_labels,
        assistant_labels,
        tts_labels,
        active: snap.active,
        discovered_stt: &snap.discovered_stt,
        update_label: snap.update_label.as_deref(),
        gpu_upgrade_label: snap.gpu_upgrade_label.as_deref(),
        microphones: (&snap.microphones.0, snap.microphones.1),
        prefs: &snap.prefs,
        mcp_server_enabled: snap.mcp_server_enabled,
        wyoming_server_enabled: snap.wyoming_server_enabled,
        llm_server_enabled: snap.llm_server_enabled,
    };
    let nodes = menu::build(&inputs);
    dispatch(move |mtm| apply_menu(mtm, &nodes));
}
