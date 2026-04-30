# Waveform Overlay for Batch (Non-Interactive) Recording

## Objective

Add an opt-in transparent waveform animation overlay that appears during batch
recording when `[interactive].enabled = false`. The overlay shows a scrolling
amplitude bar chart driven by real-time RMS levels sampled from the
`RecordingBuffer`, giving users visual feedback equivalent to what they get in
interactive mode — without activating the streaming dictation pipeline. The
feature is gated by a new `waveform` Cargo feature and a `[overlay].waveform`
config key, both opt-in, so existing slim and non-GUI builds are unaffected.

---

## Background and Key Constraints

- The real overlay (`fono-overlay/real-window`) is currently compiled only as
  part of the `interactive` feature bundle (`crates/fono/Cargo.toml:50-57`).
  A separate `waveform` feature is required so it compiles in non-interactive
  builds.
- winit forbids creating a second `EventLoop` per process
  (`session.rs:316-321`). The overlay is spawned once at daemon startup and
  reused. The new path follows the same pattern.
- The `overlay` field on `SessionOrchestrator` and the spawn block are both
  `#[cfg(feature = "interactive")]` (`session.rs:323-340`, `516`). These guards
  must be widened to include the new `waveform` feature.
- The existing `OverlayState::Recording { db: i8 }` field is stub-hardcoded to
  `-20` and never rendered (`real.rs:148`). This plan wires it properly and
  extends the rendering to draw waveform bars when the ring buffer is populated.
- The batch-path `RecordingBuffer` is an `Arc<Mutex<RecordingBuffer>>`
  accessible from outside the capture thread (`session.rs:540-549`). RMS can
  be sampled from it on a tokio ticker without touching the audio hot path.
- `spawn_pipeline()` does not currently have an overlay handle. The handle must
  be passed through so the pipeline can hide the overlay on completion.

---

## Implementation Plan

### Phase 1 — New `waveform` Cargo Feature

- [ ] 1.1. In `crates/fono/Cargo.toml`, add a `waveform` feature entry that
  enables `fono-overlay/real-window` and nothing else. This isolates the
  window dependency from the streaming pipeline. Add it to the `default`
  feature list alongside `tray`.
- [ ] 1.2. Document in the feature comment that `waveform` and `interactive`
  both pull in `real-window` and that having both enabled simultaneously is
  safe (the overlay is spawned once; the two code paths share the same handle).

### Phase 2 — New `[overlay]` Config Section

- [ ] 2.1. In `crates/fono-core/src/config.rs`, define an `OverlayConfig`
  struct with a single `waveform: bool` field defaulting to `false`. Follow
  the existing pattern of a `Default` impl and a toml `Deserialize` derive
  with `#[serde(default)]` on the struct so a missing `[overlay]` section
  in the user's `config.toml` is treated as all-defaults.
- [ ] 2.2. Add `overlay: OverlayConfig` to the top-level `Config` struct with
  `#[serde(default)]`.
- [ ] 2.3. Expose a doc comment explaining the relationship: `waveform = true`
  requires the binary to have been compiled with the `waveform` feature;
  setting the flag in a slim build is a no-op (the overlay thread is never
  spawned).

### Phase 3 — `AudioLevel` Command and Ring Buffer

- [ ] 3.1. In `crates/fono-overlay/src/real.rs`, add a new
  `OverlayCmd::AudioLevel(f32)` variant to the `OverlayCmd` enum. The `f32`
  carries a normalised amplitude value in `[0.0, 1.0]` (caller computes RMS
  and normalises against a speech-typical ceiling before sending).
- [ ] 3.2. Add a new method `push_level(&self, amplitude: f32)` on
  `OverlayHandle` that sends `OverlayCmd::AudioLevel` and wakes the event
  loop via `EventLoopProxy::send_event`. Mirror the pattern of the existing
  `send()` helper.
- [ ] 3.3. Add a `levels: std::collections::VecDeque<f32>` field and a
  `const WAVEFORM_BARS: usize = 60` constant to the `App` struct inside
  `run_event_loop`. The deque is capped at `WAVEFORM_BARS` entries; the
  oldest entry is evicted via `pop_front` when full.
- [ ] 3.4. Handle `OverlayCmd::AudioLevel(v)` in `App::about_to_wait`: push
  `v` onto `self.levels`, cap the deque, and set `needs_redraw = true` when
  the state is `Recording { .. }`.

### Phase 4 — Waveform Rendering

- [ ] 4.1. Add layout constants for the compact waveform panel:
  `WAVEFORM_WIN_WIDTH: f32 = 320.0`, `WAVEFORM_WIN_HEIGHT: f32 = 76.0`.
  These are smaller than the text overlay (`WIN_WIDTH = 640.0`,
  `WIN_MIN_HEIGHT = 80.0`) to keep the waveform widget unobtrusive.
- [ ] 4.2. Add a `waveform_mode: bool` flag to the `App` struct, initialised
  from a new `spawn_waveform` boolean passed into `run_event_loop`. Expose a
  corresponding `RealOverlay::spawn_waveform()` constructor alongside the
  existing `RealOverlay::spawn()`. Both spawn the same event loop; the flag
  selects the rendering branch.
- [ ] 4.3. In `App::resumed`, select window initial size from `waveform_mode`:
  use `WAVEFORM_WIN_WIDTH × WAVEFORM_WIN_HEIGHT` when true, otherwise keep
  the existing `WIN_WIDTH × WIN_MIN_HEIGHT`.
- [ ] 4.4. Add a `draw_waveform_bars()` free function (or inline block inside
  `redraw`) with this algorithm:
  - Compute the drawable bar area: `x0 = (PADDING_X + ACCENT_WIDTH) * scale`,
    `x1 = w as f32 - PADDING_X * scale`, `y_top` immediately below the status
    label baseline, `y_bot = h as f32 - PADDING_BOT * scale`.
  - `n = levels.len()`, skip if 0.
  - `bar_w = (x1 - x0) / n as f32`, `gap = 1.0 * scale` between bars.
  - For each bar `i` (left = oldest), height = `levels[i] * (y_bot - y_top)`.
    Use `fill_round_rect` (already in scope) for each bar rect with a
    corner radius of `2.0 * scale`.
  - Bar colour: blend between a dim version of the accent colour (alpha ~0x30,
    for silence) and the full accent colour (alpha 0xFF) linearly against the
    amplitude. This produces bars that glow brighter at higher volumes without
    going opaque-white.
  - Draw a 1-physical-pixel floor line at `y_bot` in `COLOR_TEXT_DIM` so the
    baseline is visible even at near-zero amplitude.
- [ ] 4.5. In `redraw()`, after drawing the accent stripe and status label,
  branch on `app.waveform_mode && !app.levels.is_empty()`: call
  `draw_waveform_bars()` instead of (not in addition to) the text-rendering
  block. When the deque is empty (recording just started, no levels yet) fall
  through to the existing accent-stripe-only render so the panel doesn't
  flash blank on first show.
- [ ] 4.6. Suppress the dynamic height-resize logic (`needs_resize`) when
  `waveform_mode` is true — the waveform panel stays at fixed height.

### Phase 5 — Decoupling Overlay Availability from `interactive`

- [ ] 5.1. In `crates/fono/src/session.rs`, change every
  `#[cfg(feature = "interactive")]` guard that covers the `overlay` field,
  the `with_overlay` builder method, and the spawn block to
  `#[cfg(any(feature = "interactive", feature = "waveform"))]`.
- [ ] 5.2. Extend the overlay spawn block (`session.rs:323-340`) with an
  `else if` branch: when `!config.interactive.enabled` and
  `cfg!(feature = "waveform")` and `config.overlay.waveform`, call
  `fono_overlay::RealOverlay::spawn_waveform()` instead of
  `RealOverlay::spawn()`. The resulting handle is stored in the same
  `self.overlay` slot.
- [ ] 5.3. In `spawn_pipeline()`, clone the overlay handle (when the cfg gate
  is active) and pass it into the spawned async block. On pipeline completion
  — success, empty-transcript, or failure — call
  `overlay.set_state(OverlayState::Hidden)` before returning.

### Phase 6 — Level Ticker in the Batch Recording Path

- [ ] 6.1. Add an `level_task: Option<tokio::task::AbortHandle>` field to the
  `CaptureSession` struct. This tracks the per-session level polling task.
- [ ] 6.2. At the end of `on_start_recording()`, after the `CaptureSession` is
  stored in `self.capture`:
  - Check the cfg gate and whether an overlay handle is present and in the
    `Recording` state context.
  - Clone `Arc<Mutex<RecordingBuffer>>` from the just-stored session.
  - Clone the `OverlayHandle`.
  - Spawn a `tokio::spawn` task: loop with `tokio::time::sleep(50ms)` cadence.
    Each iteration: lock the buffer, read the last 800 samples (50 ms at
    16 kHz), release the lock, compute
    `rms = sqrt(sum(s*s)/n)`, normalise to `[0.0, 1.0]` with a ceiling of
    `0.04` (empirically reasonable for typical near-field speech), call
    `overlay.push_level(normalised)`. Break when the `AbortHandle` fires.
  - Store the `AbortHandle` in `CaptureSession.level_task`.
  - Set `overlay.set_state(OverlayState::Recording { db: 0 })` to make the
    overlay visible (the `db` field is kept for API compatibility but is now
    superseded by the ring buffer for rendering purposes).
- [ ] 6.3. In `on_stop_recording()`, before calling `stop_and_drain()`: take
  the level task abort handle from the session and call `.abort()`. Then call
  `overlay.set_state(OverlayState::Processing)` so the panel transitions to
  amber while STT runs.
- [ ] 6.4. In `on_cancel()`: similarly abort the level task and call
  `overlay.set_state(OverlayState::Hidden)` immediately (no pipeline, no
  amber phase).

### Phase 7 — Public API Surface on `OverlayHandle` Stub

- [ ] 7.1. Add a `push_level(&self, _amplitude: f32)` no-op method to the
  stub `Overlay` struct in `crates/fono-overlay/src/lib.rs` so calling code
  behind the cfg gate compiles whether or not `real-window` is active. Mirror
  the existing `set_state` / `update_text` no-op pattern.

### Phase 8 — Configuration Documentation

- [ ] 8.1. Add a `[overlay]` section to the generated default config template
  (wherever `Config::default_toml()` or equivalent is produced) with
  `waveform = false` and a comment: "Set to true to show a transparent
  waveform animation during recording. Requires the `waveform` Cargo feature
  (included in default builds). Has no effect when
  `[interactive].enabled = true` (the interactive overlay is used instead)."

---

## Verification Criteria

- With `[overlay] waveform = true` and `[interactive] enabled = false`, a
  visible semi-transparent panel appears at the bottom-centre of the screen
  immediately on hotkey press, animates amplitude bars during speech, and
  disappears when STT finishes.
- With both flags false (the default), no overlay window is created at daemon
  startup — daemon behaviour is identical to the current baseline.
- With `[interactive] enabled = true`, the existing interactive overlay takes
  precedence regardless of `[overlay] waveform`.
- Bars respond to amplitude: loud speech fills bars to near-full height; silence
  shows near-zero bars (not invisible — the 1px floor remains).
- The overlay does not steal focus from the active window (existing
  `with_active(false)` + `with_override_redirect(true)` invariants are
  maintained).
- A slim build (`--no-default-features --features tray`) compiles cleanly — the
  waveform feature is not in that flag set.
- `cargo clippy --all-features` and `cargo test --all-features` pass without new
  warnings.

---

## Potential Risks and Mitigations

1. **winit EventLoop singleton conflict when both features are active**
   If `interactive` and `waveform` are both compiled in and `interactive.enabled`
   is true, the existing interactive overlay spawn takes the `if` branch; the
   waveform `else if` is never reached. Both paths produce a `RealOverlay`
   handle. No second `EventLoop` is created. However, if `interactive.enabled`
   is false and `overlay.waveform` is true, `spawn_waveform()` is called and
   uses the same event-loop-spawning code. The singleton constraint is
   satisfied because only one branch runs.
   Mitigation: Document this in a code comment at the spawn site. Add an
   assertion or `tracing::warn!` if both would fire simultaneously.

2. **Audio hot-path interference from buffer sampling**
   The level ticker holds `Mutex<RecordingBuffer>` for a short read every 50 ms.
   The cpal callback also locks this mutex every ~10 ms. Lock contention is
   possible but brief (a memcpy of 800 f32 values = ~3 µs).
   Mitigation: The ticker computes RMS from the tail of `samples()` — no copy
   needed if the RMS loop iterates the slice directly while the lock is held.
   This keeps the critical section to ~3 µs, well below the cpal interrupt
   budget.

3. **Wayland focus behaviour**
   The X11 `override_redirect` fix that prevents focus theft
   (`real.rs:500-506`) does not apply on Wayland. This risk is identical to
   the existing interactive overlay and is deferred to Slice B per ADR 0009.
   Mitigation: Document in a comment that on Wayland the waveform panel may
   briefly capture focus on compositors that do not honour
   `with_active(false)`. The Slice B subprocess refactor addresses both the
   interactive and waveform panels simultaneously.

4. **Amplitude normalisation ceiling tuning**
   A ceiling of `0.04 RMS` may be too conservative (bars always near-full for
   loud speakers) or too generous (bars stay half-height for quiet
   microphones). The value is not user-configurable in v1.
   Mitigation: Make the normalisation ceiling a named constant
   `WAVEFORM_RMS_CEILING: f32 = 0.04` in `session.rs` with a comment
   explaining the rationale. A follow-up config knob (`overlay.rms_ceiling`)
   can be added once real-world feedback is collected.

5. **`spawn_pipeline` requires an additional overlay parameter**
   `spawn_pipeline` currently accepts only `pcm: Vec<f32>` and `capture_ms`.
   Threading the overlay handle through it adds coupling.
   Mitigation: Clone the `Arc<StdRwLock<Option<OverlayHandle>>>` from `self`
   inside `spawn_pipeline` (the same pattern already used for `stt`, `llm`,
   `history`, etc.) rather than adding a new argument. This avoids changing
   the function signature.

---

## Alternative Approaches

1. **Shared level atomic instead of `OverlayCmd::AudioLevel`**: Expose an
   `Arc<AtomicU32>` (bit-cast f32) from the capture crate and have the overlay
   poll it on a fixed timer instead of receiving push commands. This avoids
   channel overhead but couples `fono-overlay` to `fono-audio`, violating the
   current layered architecture where the overlay knows nothing about audio.
   Rejected in favour of the push-command approach.

2. **Timer-based redraw loop inside the overlay instead of command-push**:
   Add a `ControlFlow::WaitUntil(now + 50ms)` redraw cycle to the winit event
   loop and have the overlay fetch levels from a shared atomic. This
   centralises animation timing in the overlay crate and avoids the level
   ticker task in `session.rs`. Trade-off: requires the overlay to hold a
   reference to shared audio state, and the winit loop would fire redraws even
   when hidden. Rejected; the command-push model already used by
   `SetState`/`UpdateText` is simpler and consistent.

3. **Subprocess overlay for Wayland correctness now**: Address the Wayland
   focus issue in this same pass by moving the overlay to a child process
   (ADR 0009 §5). This was explicitly deferred to Slice B because the IPC
   complexity is disproportionate to the waveform scope. The existing
   in-process model is good enough for X11 (the primary target at this stage).

4. **Oscilloscope waveform instead of bar chart**: Draw the amplitude ring
   buffer as a continuous line (PCM samples rather than RMS per chunk),
   giving a classic oscilloscope look. This requires per-sample forwarding
   from the capture callback (more CPU, more channel messages) and a
   line-rasterisation primitive not currently in the softbuffer renderer.
   The bar chart with 60 RMS samples at 50 ms cadence (3-second window) is
   visually equivalent and far simpler to implement.
