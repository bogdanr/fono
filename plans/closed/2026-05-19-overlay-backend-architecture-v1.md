# Overlay Backend Architecture — Robust Wayland / X11 / Headless

## Status: Completed

## Objective

Replace the single `winit` + `softbuffer` overlay path with a small **pluggable
overlay-backend layer** so that:

1. The overlay renders correctly on every mainstream Linux session:
   GNOME / Mutter (Wayland), KDE / KWin (Wayland), wlroots compositors
   (sway, hyprland, river, COSMIC, Wayfire), X11 (any WM), and headless.
2. Transparency, rounded corners, bottom-centre anchoring, and "no focus
   theft" hold uniformly — not just on X11.
3. Wayland-specific dependencies (layer-shell protocol bindings, portal
   clients) are **optional at runtime**: if a backend's prerequisites are
   missing the daemon falls back to the next viable backend (down to
   "no overlay") **without aborting startup**.
4. The CPU-only Fono binary's size budget (≤ 20 MiB per ADR 0022) is
   preserved: any new backend dep tree must be small or already in
   the workspace's transitive graph (winit already pulls in `wayland-protocols-wlr`
   and `smithay-client-toolkit`).
5. The existing FFT / oscilloscope / bars / heatmap / transcript
   rendering code (`crates/fono-overlay/src/real.rs`) is preserved
   verbatim and reused by every backend — only the *windowing surface*
   varies. We're not rewriting the renderer.

## Background — what's broken today

Verified from a focused read of `crates/fono-overlay/src/real.rs` plus
the inline comments at lines 1647–1681 and 1338–1352 and the
`docs/status.md` 2026-04-28 entry:

- **Placement bug.** `xdg_toplevel` does not let clients position
  themselves. Our X11 path uses `set_outer_position` for bottom-centre
  anchoring (`crates/fono-overlay/src/real.rs:1346-1352`), which is a
  no-op on Wayland. Mutter / KWin then map the undecorated 640×80
  toplevel at compositor-default placement (top-left on GNOME, screen
  centre on KDE). User report on Ubuntu 24.04 confirms: "always lands
  on the left top side".
- **Transparency bug.** `softbuffer 0.4.x` hardcodes
  `WL_SHM_FORMAT_XRGB8888` for its Wayland `wl_shm` buffers
  (`backends/wayland/buffer.rs:102, 142`). The alpha byte is dropped at
  composite time regardless of `with_transparent(true)`. Our workaround
  at `crates/fono-overlay/src/real.rs:1666-1668` clears the buffer to
  opaque BG on Wayland — visible as a solid charcoal rectangle with no
  rounded corners. User report confirms: "it's not transparent".
- **Hide-on-Idle bug.** `Window::set_visible(false)` is a no-op on
  `xdg_toplevel`. Our current mitigation drops the `winit::Window` and
  recreates it on each show, which re-triggers the Mutter
  default-placement bug on every dictation.
- **No-input bug (latent).** `xdg_toplevel` has no notion of an
  input-passthrough region. The overlay can theoretically eat clicks
  meant for the underlying app on Wayland. Today this only manifests
  on focus theft and is masked because the overlay quickly disappears.

The only Wayland protocol that solves all three at once is
[`wlr_layer_shell_unstable_v1`](https://wayland.app/protocols/wlr-layer-shell-unstable-v1):
client-anchored placement (Bottom + horizontal-centre), `layer = OVERLAY`
or `TOP`, configurable input region (none → pass-through), keyboard
interactivity off. Supported by sway, hyprland, river, COSMIC, KWin
(KDE Plasma 5.27+), and Wayfire. **Not** supported by Mutter / GNOME
Shell — GNOME upstream has refused to implement it for a decade and
will not in the foreseeable future.

GNOME has no equivalent first-party protocol. The realistic options on
Mutter are:

- **GNOME Shell extension.** A small JS extension exposes a D-Bus
  service that Fono drives ("show overlay at bottom centre with this
  bitmap"). Heavy operator burden (user must install + enable an
  extension); ruled out for Phase 1.
- **`xdg_toplevel` with ARGB `wl_shm` format + best-effort positioning
  via hints.** Mutter centres small undecorated toplevels horizontally
  but leaves them mid-screen vertically; clients can't change that.
  Acceptable graceful fallback: an ARGB toplevel that paints correctly
  but sits where Mutter puts it, with a small user-visible
  "centred" / "anchored" indicator in `fono doctor` so the user knows
  why placement is off.
- **`xdg_activation_v1` + portal `Window.Move`.** Vapourware on
  Mutter as of 2026-05.

So the Phase 1 deliverable splits into two tiers:

- **First-class Wayland support** via `wlr-layer-shell` on every
  compositor that implements it (sway, hyprland, river, KDE,
  COSMIC, Wayfire, Wayfire-derivatives). Identical UX to X11.
- **Graceful Wayland fallback** on Mutter (and any future compositor
  without layer-shell) via raw `smithay-client-toolkit` + ARGB
  `wl_shm`. Transparency and rounded corners work; placement is
  compositor-chosen and documented in `docs/wayland.md` as a known
  limitation.

A GNOME-Shell-extension path is acceptable as a future Phase-3
opt-in but is **not** on the critical path.

## Design — backend trait + runtime selection

### 1. The renderer is unchanged

`crates/fono-overlay/src/real.rs` contains ~1900 lines of
software-rasterised drawing (rounded panel, FFT bars, oscilloscope,
heatmap cache, VU bar, glyph drawing). This code operates on a
`&mut [u32]` ARGB premultiplied framebuffer. **Every backend hands
the renderer the same `(buf: &mut [u32], width: u32, height: u32)`
contract.** No backend touches drawing logic; the renderer never
touches windowing.

A focused refactor splits `real.rs` into:

- `crates/fono-overlay/src/renderer.rs` — pure pixel-pushing. Inputs:
  `OverlayState`, `WaveformStyle`, current text, ring buffers, scale
  factor, framebuffer. No `winit`, no `softbuffer`. Unit-testable.
- `crates/fono-overlay/src/backend.rs` — the `OverlayBackend` trait
  + selection logic (see §2).
- `crates/fono-overlay/src/backends/winit_x11.rs` — current
  `winit` + `softbuffer` path, X11-only feature-gated.
- `crates/fono-overlay/src/backends/wayland_layer_shell.rs` — new
  primary Wayland path.
- `crates/fono-overlay/src/backends/wayland_xdg.rs` — Wayland
  fallback for Mutter and any other layer-shell-less compositor.
- `crates/fono-overlay/src/backends/noop.rs` — present today as
  the slim-build stub; promoted to a first-class fallback.

### 2. The backend trait

```rust
// SPDX-License-Identifier: GPL-3.0-only
pub trait OverlayBackend: Send {
    /// Construct the backend. Returns `Err` if the environment is
    /// not viable (no DISPLAY/WAYLAND_DISPLAY, missing protocol,
    /// missing library at runtime, etc.). Callers fall through to
    /// the next backend on `Err`.
    fn try_new(style: WaveformStyle) -> io::Result<Self>
    where
        Self: Sized;

    /// Pump platform events + flush pending commands. Called on
    /// every tick of the overlay thread.
    fn pump(&mut self, cmd_rx: &mpsc::Receiver<OverlayCmd>) -> Result<PumpOutcome, BackendError>;

    /// Borrow a writable ARGB-premultiplied framebuffer at
    /// `(width, height)` and present it. Renderer pushes pixels;
    /// the backend does whatever surface mechanics are needed.
    fn with_framebuffer<F: FnOnce(&mut [u32], u32, u32, f32 /*scale*/)>(
        &mut self,
        f: F,
    ) -> Result<(), BackendError>;

    /// Show / hide the surface. Hide must actually unmap (drop
    /// surface on xdg_toplevel; layer-surface destroy on wlr;
    /// XUnmapWindow on X11; no-op on no-op backend).
    fn set_visible(&mut self, visible: bool);

    /// Stable identifier for `fono doctor` ("wlr-layer-shell",
    /// "wayland-xdg-fallback", "x11-override-redirect", "noop").
    fn name(&self) -> &'static str;

    /// Reported capabilities — used by `fono doctor` and the
    /// "graceful degradation" log line at startup.
    fn capabilities(&self) -> BackendCapabilities;
}

pub struct BackendCapabilities {
    pub transparency: bool,
    pub client_positioning: bool, // false on Wayland-xdg-fallback
    pub focus_passthrough: bool,
    pub click_passthrough: bool,
}
```

`OverlayCmd` is the existing enum at
`crates/fono-overlay/src/real.rs:62-90`; no schema change required.

### 3. Backend selection — deterministic, env-driven, log-loud

`OverlayHandle::spawn(style)` is the existing public entry point
(`crates/fono-overlay/src/real.rs:182`). Its body becomes:

1. Inspect environment variables in priority order. `FONO_OVERLAY_BACKEND=...`
   forces a specific backend (operator override for diagnostics).
2. Otherwise compute the **candidate list** from session signals:

   | Session signal | Candidate order |
   |---|---|
   | `WAYLAND_DISPLAY` set, compositor advertises `zwlr_layer_shell_v1` | `wlr-layer-shell` → `wayland-xdg-fallback` → `noop` |
   | `WAYLAND_DISPLAY` set, no layer-shell | `wayland-xdg-fallback` → `noop` |
   | `DISPLAY` set, no `WAYLAND_DISPLAY` | `x11-override-redirect` → `noop` |
   | Both set (Xwayland) | Prefer Wayland-native (`wlr-layer-shell` → `wayland-xdg-fallback` → `x11-override-redirect` → `noop`) so the overlay isn't double-composited through Xwayland. |
   | Neither set | `noop` |

3. Iterate the candidate list, calling `try_new()`. The first
   `Ok` wins. Every failure logs at `info` (`overlay: backend X
   not viable: <reason>; trying Y`) so we have field
   diagnostics. The final outcome is logged at `info` with backend
   name + capability summary, also surfaced by `fono doctor`.

The "should not prevent fono to start" requirement is satisfied
by treating the `noop` backend as a terminal sink: even on a
completely unknown / broken environment, `OverlayHandle::spawn`
always returns `Ok` with a working (silent) handle. The daemon
continues with audio capture, hotkeys, STT, polish, and injection
intact — exactly the "headless mode" path that already exists for
`fono.service` on inference boxes.

### 4. wlr-layer-shell backend (the primary Wayland path)

Implementation sketch using `smithay-client-toolkit 0.19` (already
indirectly in the workspace via `winit`) and `wayland-protocols-wlr`
(also already there per `Cargo.lock:4870`):

- Connect to the compositor via `Connection::connect_to_env`.
- Bind `wl_compositor`, `wl_shm`, `zwlr_layer_shell_v1`,
  `zxdg_output_manager_v1`, and `wl_seat` (for HiDPI scale via
  `wl_output`).
- Create a `wl_surface`; wrap it via `layer_shell.get_layer_surface`
  with `layer = Top` (not `Overlay` — we don't want to cover the
  user's notifications), namespace `"fono"`.
- Anchor: `Bottom | Left | Right` so the surface spans the bottom
  edge and we can centre our content via margins.
- Size: 640 × N (where N is the dynamic content height the renderer
  decides), margin-bottom 48 px.
- `set_keyboard_interactivity(None)` — no focus theft, ever.
- `set_input_region(empty_region)` — clicks pass through to the
  underlying app. (Renderer-level visuals only.)
- `wl_shm` pool with `ARGB8888` format (the standard, mandatory
  Wayland format with alpha — softbuffer 0.4.x is the only thing
  that gets this wrong, by hardcoding XRGB). Allocate via
  `memfd_create` + `mmap` so resizes are cheap.
- Handle `configure` events: ack, resize the shm pool, re-render.
- Handle `wl_output.scale` for HiDPI; the renderer already takes a
  `scale` factor.

Approximate added LOC: 350–500 in `wayland_layer_shell.rs`. No new
crates introduced; all needed protocol bindings already transitively
present. Net binary-size delta expected to be **negative** once we
drop the winit Wayland event-loop path on `wlr-layer-shell`-capable
sessions (winit's Wayland support is heavy).

### 5. wayland-xdg-fallback backend (Mutter / GNOME)

When the compositor doesn't advertise `zwlr_layer_shell_v1`, fall
back to raw `xdg_toplevel` over the same smithay-client-toolkit
plumbing as §4, but with the protocol's inherent limitations:

- Same ARGB8888 `wl_shm` buffer → transparency + rounded corners
  work (fixes "it's not transparent").
- `xdg_toplevel.set_app_id("fono")` and `.set_title("Fono")`.
- Client-side decoration off; `xdg_decoration` requests SSD if the
  compositor offers it, otherwise none — undecorated is fine.
- **Placement is compositor-chosen.** On Mutter the surface lands
  near the centre of the active monitor, not at the bottom. This
  is a known Mutter limitation and we cannot fix it in user space.
- `fono doctor` notes: `overlay backend: wayland-xdg-fallback
  (compositor controls placement)`. The Wayland doc gets a section
  noting that the overlay floats wherever GNOME parks it and
  pointing users at the GNOME-Shell-extension Phase 3 path if /
  when it ships.
- Hide actually unmaps by destroying the toplevel (same approach as
  today, but with a backend that owns the surface state cleanly).

Approximate added LOC: 250–300, sharing the `wl_shm` buffer
plumbing with §4.

### 6. X11 backend (carryover, no behavioural change)

The existing `winit` + `softbuffer` X11 path stays. It already
handles override-redirect, transparency, and bottom-centre anchoring
correctly. We move it behind the `OverlayBackend` trait and call it
`x11-override-redirect`. No code changes inside the renderer.

Once the Wayland backends are first-class, a follow-up phase can
evaluate dropping `winit` entirely (replacing the X11 path with
raw `x11rb` would let us drop winit's full event-loop dep tree —
several MB of binary). Not in Phase 1.

### 7. No-op backend (graceful degradation)

The `Overlay` stub at `crates/fono-overlay/src/lib.rs:56-112` is
promoted to implement `OverlayBackend`. `OverlayCmd`s are
acknowledged silently; `set_state` / `update_text` are logged at
trace level. Result: any daemon, on any environment, on any kernel
/ compositor combination, *always* gets an `OverlayHandle` back
from `spawn()` and the rest of the pipeline runs uninterrupted.

### 8. Subprocess sandbox — **deferred to Phase 2**

ADR 0009 §5 already flags the long-term goal of moving the overlay
into a subprocess so a graphics-driver wedge can't take down the
daemon. The backend abstraction lands first because:

- Subprocess IPC is its own design exercise (frame protocol, shared
  memory vs unix-socket pixel push, lifecycle, restart policy).
- The Wayland brokenness the user is reporting is *not* solved by
  subprocessing — putting a broken renderer in a subprocess is
  still a broken renderer.
- The current crash-isolation story (winit failure → empty handle)
  is already acceptable for v0.x given Wayland is the dominant
  failure surface and the renderer itself has been stable for
  months.

Phase 2 lifts the `OverlayBackend` implementations into a
`fono-overlay-helper` binary launched on demand. The trait stays
identical; the in-process call site becomes an IPC client. The
helper binary inherits the daemon's parent-death signal
(`prctl(PR_SET_PDEATHSIG, SIGTERM)`) so an orphaned overlay can
never outlive the daemon.

## Binary size budget

The single biggest risk in this work is bloating the CPU binary
past the 20 MiB CI gate (ADR 0022). Pre-mitigations:

- **No new crates.** `smithay-client-toolkit`,
  `wayland-protocols-wlr`, `wayland-client` are all already in
  `Cargo.lock` via `winit`. Adding them as direct deps is free.
- **Cargo feature partitioning.** Backend modules behind cargo
  features (`backend-wlr`, `backend-wayland-xdg`, `backend-x11`),
  all enabled by default in the desktop release but trimmable for
  the server build. The trait + dispatch + noop are always
  compiled.
- **Replace winit on Wayland.** Once the SCTK-based backends are
  proven, the winit Wayland event loop is unused. The follow-up
  size win is dropping winit's `wayland*` features, which the
  workspace currently inherits transitively. Conservative estimate:
  −1.5 to −2.5 MiB stripped.
- **Renderer split is pure code motion.** No new code, no new deps.

Acceptance: the size-budget CI job
(`.github/workflows/ci.yml:size-budget`) must remain green on the
default CPU variant. If it goes red we trim winit features or feature-
gate one of the new backends.

## Implementation Plan

### Phase 0 — refactor (no behaviour change)

- [ ] Task 0.1. Split `crates/fono-overlay/src/real.rs` into
  `renderer.rs` (pure drawing) + `backend.rs` (trait) + `backends/winit_x11.rs`
  (current code, unchanged). Net diff: pure file moves + a thin trait
  impl. Verify `cargo test -p fono-overlay --features real-window`
  stays green and the produced overlay is pixel-identical to the
  pre-refactor build on X11.
- [ ] Task 0.2. Promote `Overlay` (slim stub) to implement
  `OverlayBackend`. Wire the `spawn` path through the trait via a
  `Box<dyn OverlayBackend>`. Confirm the slim build (no `real-window`)
  still compiles and the daemon still starts headless.

### Phase 1 — Wayland first-class support

- [ ] Task 1.1. **Promote `smithay-client-toolkit` and
  `wayland-protocols-wlr` to direct workspace deps.** Add to
  `crates/fono-overlay/Cargo.toml` under new `backend-wlr` and
  `backend-wayland-xdg` features (both default-on when `real-window`
  is on). Verify `cargo tree -p fono-overlay --features
  backend-wlr,backend-wayland-xdg` shows no *new* leaf crates beyond
  what `winit` already pulled in.
- [ ] Task 1.2. **Implement `backends/wayland_layer_shell.rs`** per
  §4. Includes: `wl_registry` bind walk, ARGB8888 shm pool with
  `memfd_create`, `zwlr_layer_surface_v1.configure` handling, HiDPI
  scale via `wl_output`, bottom-centre anchor, empty input region,
  `set_keyboard_interactivity(None)`. Render path calls the renderer
  module produced by Task 0.1.
- [ ] Task 1.3. **Implement `backends/wayland_xdg.rs`** per §5.
  Same shm machinery as Task 1.2 (factor into a private
  `wayland_shm.rs` helper module); `xdg_wm_base` + `xdg_toplevel`
  instead of `zwlr_layer_surface_v1`; document the placement
  limitation in a top-of-file comment so future agents don't
  "fix" it by trying to set position.
- [ ] Task 1.4. **Backend selection logic** per §3 lands in
  `backend.rs`. Adds `FONO_OVERLAY_BACKEND` env override. Logs the
  full candidate-walk at info level on every overlay spawn.
  Includes a `probe_layer_shell()` helper that does a *fast*
  registry bind without creating a real surface, used to choose
  between `wlr-layer-shell` and `wayland-xdg-fallback` before the
  first overlay show.
- [ ] Task 1.5. **`fono doctor` integration.** Add an "Overlay"
  section reporting the chosen backend, its capabilities
  (transparency / positioning / passthrough), and — when
  `wayland-xdg-fallback` is selected on Mutter — a hint pointing
  users at `docs/wayland.md` for the known placement limitation.
- [ ] Task 1.6. **`docs/wayland.md` rewrite.** New section "How the
  overlay works on Wayland" covering the layer-shell vs Mutter
  split, what to expect on each compositor, the
  `FONO_OVERLAY_BACKEND` escape hatch, and a troubleshooting
  decision tree.
- [ ] Task 1.7. **Integration tests.** Add an opt-in CI lane that
  runs the daemon under `weston --backend=headless-backend.so`
  (a real wlr-layer-shell-capable compositor in CI). Asserts the
  overlay binds the layer-shell protocol and renders a frame.
  Gracefully skipped on workflows without weston.
- [ ] Task 1.8. **Size-budget verification.** Run the size-budget
  job locally on a glibc ubuntu-22.04 build. If it overshoots
  20 MiB, trim by disabling winit's `wayland*` features in
  `Cargo.toml` (`winit = { default-features = false, features = [
  "x11", ...] }`) — winit only needs to handle the X11 path now.

### Phase 2 — subprocess isolation (deferred — separate plan when Phase 1 is in)

- [ ] Task 2.1. Define the IPC contract (unix-socket framed
  protobuf-style or bincode; `OverlayCmd` already serialisable).
- [ ] Task 2.2. Extract the backends into a `fono-overlay-helper`
  bin target inside `crates/fono-overlay`. Launched by the daemon
  with `PR_SET_PDEATHSIG`.
- [ ] Task 2.3. Restart policy: on helper crash, daemon logs at
  warn and re-spawns once per 10 s, capped at 3 attempts per
  session.

### Phase 3 — GNOME Shell extension (deferred, ecosystem-bet, optional)

- [ ] Task 3.1. Write a 200-line GNOME Shell JS extension that
  exposes a `org.fono.Overlay` D-Bus service accepting
  `ShowFrame(bytes argb_buffer, int x, int y, int w, int h)`.
- [ ] Task 3.2. Add a `backend-gnome-shell` backend that calls
  the service when the extension is installed and enabled; falls
  through to `wayland-xdg-fallback` when absent. Publish the
  extension to extensions.gnome.org as a discoverable opt-in.
  Skip entirely if upstream Mutter ever ships
  `xdg_activation_v1`-based positioning or layer-shell.

## Verification Criteria

- On Ubuntu 24.04 (GNOME 46 / Mutter / Wayland — the
  `192.168.0.112` reproducer), the overlay during dictation:
  - is transparent with rounded corners (verified by eye against
    a coloured wallpaper),
  - does not steal keyboard focus from the previously-active
    window (verified by typing into a terminal during a
    push-to-talk session and confirming keystrokes land in the
    terminal),
  - does not steal pointer clicks (verified by clicking through
    the overlay onto an underlying window),
  - shows up in a predictable position. On Mutter "predictable"
    means "wherever the compositor decided", documented up front;
    on every other Wayland desktop it means "bottom centre, 48 px
    above the bottom edge".
- On Fedora 40 KDE Plasma 6 (KWin / Wayland), the overlay
  renders at bottom-centre using the `wlr-layer-shell` path.
- On sway / hyprland (wlroots), same bottom-centre layer-shell
  behaviour as KDE.
- On X11 (NimbleX i3, Ubuntu 22.04 GNOME-on-Xorg, Fedora KDE-X11),
  no regression vs the current `winit` + `softbuffer` path.
- On a host with neither `DISPLAY` nor `WAYLAND_DISPLAY` (systemd
  service mode), `fono` starts cleanly with the `noop` backend; no
  warning escalations.
- `fono doctor` prints the selected backend + capability summary on
  every supported environment.
- The CPU binary stays ≤ 20 MiB after the change, with the
  size-budget CI gate green.
- The clippy + fmt + workspace test gate stays green:
  `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`.

## Potential Risks and Mitigations

1. **Binary-size regression.** Adding two Wayland backends on top of
   winit could push the binary over the 20 MiB CPU budget.
   Mitigation: backend modules behind cargo features (`backend-wlr`,
   `backend-wayland-xdg`); once Wayland is on SCTK, trim winit's
   `wayland*` features so winit is X11-only on the desktop release.
   The Wayland protocol crates are already in the lockfile, so the
   added text is mostly our own ~600–800 LOC of glue.

2. **Mutter UX still degraded after the fix.** The `wayland-xdg-fallback`
   backend solves transparency and crash-free hide/show but cannot
   solve placement. Mitigation: be loud about it (`fono doctor`
   message, `docs/wayland.md` section, log line at startup). Treat
   GNOME-Shell-extension Phase 3 as the long-term placement story.

3. **smithay-client-toolkit version skew.** SCTK has had API
   churn between 0.18 / 0.19 / 0.20. Mitigation: pin the version
   in `Cargo.toml` and document the chosen version in the
   backend's top-of-file comment; bump deliberately in its own
   commit when needed.

4. **HiDPI scaling mismatch between backends.** wlr-layer-shell and
   xdg-toplevel report scale differently than winit / softbuffer.
   Mitigation: the renderer already takes a `scale: f32` parameter
   (see `crates/fono-overlay/src/real.rs:1689`); each backend
   computes its own scale from the relevant wayland-protocol object
   (`wl_output.scale` or `wl_surface.preferred_buffer_scale`) and
   passes it through unchanged.

5. **Compositor-specific layer-shell quirks.** KWin's layer-shell
   implementation has historically diverged from sway's reference
   in edge cases (anchor + margin interaction, `Top` vs `Overlay`
   z-order). Mitigation: stick to the most conservative subset
   (Bottom + horizontal anchor for centring; `Top` layer, not
   `Overlay`); manual smoke-test on at least sway, KDE Plasma 6,
   and hyprland before tagging.

6. **Sand-boxed Flatpak / Snap.** Sandboxed Fono builds may not
   have access to the Wayland socket if the manifest doesn't
   grant it. Mitigation: out of scope for v0.x (we don't ship a
   Flatpak), but the `noop` backend ensures the daemon still
   starts cleanly inside a misconfigured sandbox.

7. **Subprocess overlay landing later increases attack surface.**
   The Phase 1 design keeps overlay code in-process; a future
   driver-level wedge could still take down the daemon.
   Mitigation: Phase 2 in this plan + the same fallback path the
   slim build already uses (no overlay = no daemon crash).

## Alternative Approaches

1. **GTK4-Layer-Shell + GTK4 renderer.** Drop softbuffer entirely
   and use GTK4 with the `gtk4-layer-shell` helper to drive the
   overlay. Trade-off: pulls GTK4 + cairo + pango + gdk-pixbuf
   into the binary (≈8–12 MiB), wiping the 18 MiB budget
   permanently. ADR 0022 explicitly rejected GTK for the tray for
   exactly this reason. **Rejected.**

2. **wgpu-based renderer.** Replace softbuffer with wgpu + a
   wgpu-on-wlr-layer-shell adapter. Gives us GPU-accelerated text
   + visualisations. Trade-off: wgpu adds ~6 MiB to the binary,
   another ~10 MiB transitively via shader compilation, and a
   Vulkan/Metal/D3D12 runtime dependency on the host. The CPU
   build is supposed to run on hosts *without* a working Vulkan
   ICD (that's the whole point of the CPU variant per ADR 0022
   amendment). **Rejected for the CPU variant; possibly worth
   revisiting for the GPU variant in a separate plan once the
   FFT renderer becomes a perf bottleneck (it currently is not —
   ~13–15 % of one CPU core per overlay; see the v0.6.0 release
   note in `docs/status.md`).**

3. **Ship a tiny C overlay binary using cairo + wlr-layer-shell
   directly.** The Wayland reference language is C. Trade-off:
   reintroduces a non-Rust toolchain dep on every build host,
   contradicts ADR 0019's "single Rust toolchain" promise.
   **Rejected.**

4. **Skip Wayland entirely and document "use X11 / Xwayland".**
   Trade-off: works *today* (Xwayland honours the override-redirect
   path), but every major distro is moving to Wayland-by-default
   and the user reporting this bug is on a Wayland session. We
   would be punting the problem permanently and would also lose
   the input-passthrough win that layer-shell gives us for free.
   **Rejected.**

5. **Fork softbuffer to expose ARGB8888 on Wayland.** Has been on
   the softbuffer upstream's TODO since 2024. We could send the
   patch upstream and pin a fork until it lands. Trade-off: still
   leaves the `xdg_toplevel` placement bug untouched (a
   compositor-level limitation, not a softbuffer one). Solves only
   one of the three reported issues. **Rejected as a standalone
   fix; the patch may be worth submitting upstream as a side
   benefit of the SCTK migration.**
