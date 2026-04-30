# Waveform Overlay for Batch Recording + Interactive Signal Level Bar

## Objective

1. **Standalone waveform overlay** (when `[interactive].enabled = false`): a full-size transparent panel — same 640 px-wide, bottom-centre geometry as the interactive overlay — that displays one of three selectable audio visualisation styles (`bars`, `oscilloscope`, `pulse`) during batch recording.
2. **Interactive overlay signal bar**: a narrow vertical VU bar on the right side of the existing interactive overlay panel, always visible during `LiveDictating`, showing real-time microphone signal level so users can monitor voice quality without disrupting the transcript text area.

Both features are driven by the same `OverlayCmd::AudioLevel(f32)` command. The oscilloscope style additionally requires a higher-resolution `OverlayCmd::AudioSamples(Vec<f32>)` command carrying raw PCM batches.

---

## Background and Key Constraints

### GUI-only — not available in server mode

Both features described in this plan (`[overlay] waveform` and `[overlay] volume_bar`) require a graphical compositor and a display server. They are **not** available in any headless or server deployment of Fono. Fono will eventually support a pure server mode (no tray, no hotkey, no overlay) where it accepts audio over the network and returns transcripts — in that build neither `winit` nor `softbuffer` is compiled in. The `waveform` feature must therefore be absent from any future `server` feature profile; the `[overlay]` config section is silently ignored when the `real-window` feature is not compiled in (the no-op stub methods on `Overlay` ensure callers compile without changes). Document this clearly in the `[overlay]` config section comment.

### Technical constraints

- `real-window` is currently compiled only via the `interactive` feature (`crates/fono/Cargo.toml:50-57`). A new `waveform` feature is needed so the overlay compiles without activating the streaming pipeline.
- winit forbids a second `EventLoop` per process (`session.rs:316-321`). Both interactive and waveform modes share the same long-lived handle; only one spawn path fires at startup.
- The overlay fields and spawn block are `#[cfg(feature = "interactive")]` (`session.rs:323-340`, `516`). These guards must be widened to `any(feature = "interactive", feature = "waveform")`.
- The standalone overlay uses the **same geometry** as the interactive overlay: `WIN_WIDTH = 640.0`, `WIN_MIN_HEIGHT = 80.0`, `WIN_MAX_HEIGHT = 240.0`, bottom-centre with `BOTTOM_OFFSET = 48`. No new size constants are needed.
- In interactive mode the text area currently occupies `WIN_WIDTH - PADDING_X * 2.0 - ACCENT_WIDTH` wide (`real.rs:573`). Adding a right-side VU bar requires reserving additional right margin so text reflow accounts for the bar width.
- Live dictation audio is available as raw PCM chunks in the drain task (`session.rs:893-901`): `while let Some(chunk) = tokio_rx.recv().await { pump.push(&chunk); }`. This is the cleanest tap point for computing RMS without subscribing to the broadcast channel.
- Batch recording audio is in `Arc<Mutex<RecordingBuffer>>` accessible after capture starts (`session.rs:549`). A tokio ticker task polls it every ~33 ms.
- `OverlayState::Recording { db: i8 }` exists and has red accent rendering but `db` is always `-20` and ignored by the renderer (`real.rs:148`). It is superseded by the ring buffers introduced here.

---

## Implementation Plan

### Phase 1 — New `waveform` Cargo Feature

- [ ] 1.1. In `crates/fono/Cargo.toml`, add `waveform = ["fono-overlay/real-window"]` as a standalone feature and include it in the `default` feature list alongside `tray`. Add a comment stating that `waveform` and `interactive` both activate `real-window`; having both enabled simultaneously is safe because only one spawn path fires at daemon startup.

### Phase 2 — Config: `[overlay]` Section

- [ ] 2.1. In `crates/fono-core/src/config.rs`, define a `WaveformStyle` enum with three variants: `Bars`, `Oscilloscope`, `Pulse`. Derive `Deserialize`, `Default` (= `Bars`), `Debug`, `Clone`, `Copy`, `PartialEq`.
- [ ] 2.2. Define an `OverlayConfig` struct with:
  - `waveform: bool` — default `false`. Enables the standalone waveform overlay during batch recording.
  - `style: WaveformStyle` — default `WaveformStyle::Bars`. Selects the visualisation style; only used when `waveform = true`.
  - `volume_bar: bool` — default `true`. Shows the right-side VU bar in the interactive overlay during live dictation.
  Derive `Deserialize`, `Default`, `Debug`, `Clone`. Use `#[serde(default)]` on the struct.
- [ ] 2.3. Add `overlay: OverlayConfig` with `#[serde(default)]` to the top-level `Config` struct.
- [ ] 2.4. Document `volume_bar` with a note that it has no effect when the `interactive` feature or `real-window` feature is not compiled in.

### Phase 3 — New Overlay Commands

- [ ] 3.1. In `crates/fono-overlay/src/real.rs`, add two new `OverlayCmd` variants:
  - `AudioLevel(f32)` — normalised RMS amplitude in `[0.0, 1.0]`. Used by all three visualisation styles for the bar/pulse ring buffer, and by the interactive overlay's VU bar.
  - `AudioSamples(Vec<f32>)` — a batch of raw f32 PCM samples (16 kHz mono). Used only by the `Oscilloscope` style to populate its high-resolution sample ring buffer. Kept separate from `AudioLevel` so the oscilloscope's higher-cadence path does not bloat the bar/pulse path.
- [ ] 3.2. Add two corresponding public methods on `OverlayHandle` — `push_level(&self, amplitude: f32)` and `push_samples(&self, samples: Vec<f32>)` — that send the respective commands and wake the event loop via `EventLoopProxy::send_event`. Mirror the pattern of the existing `send()` helper.
- [ ] 3.3. Add no-op `push_level` and `push_samples` stub methods on the `Overlay` struct in `crates/fono-overlay/src/lib.rs` so calling code compiles in non-`real-window` builds.

### Phase 4 — `App` State Additions in `real.rs`

- [ ] 4.1. Add three fields to the `App` struct inside `run_event_loop`:
  - `waveform_style: WaveformStyle` — set once at spawn time. Selects the rendering branch for the standalone overlay.
  - `levels: std::collections::VecDeque<f32>` — ring buffer of normalised RMS amplitudes, max 60 entries (= 2 seconds at 33 ms cadence). Used by `Bars` and `Pulse` styles.
  - `osc_samples: std::collections::VecDeque<f32>` — ring buffer of raw PCM samples, max 3 200 entries (= 200 ms at 16 kHz). Used by the `Oscilloscope` style.
- [ ] 4.2. Handle `OverlayCmd::AudioLevel(v)` in `App::about_to_wait`: push `v` onto `self.levels`, evict from front when full, set `needs_redraw = true` when visible.
- [ ] 4.3. Handle `OverlayCmd::AudioSamples(s)` in `App::about_to_wait`: extend `self.osc_samples` with the incoming slice, evict from the front to stay within the 3 200-entry cap, set `needs_redraw = true` when visible and style is `Oscilloscope`.
- [ ] 4.4. Expose `waveform_style` through `RealOverlay`: add `RealOverlay::spawn_waveform(style: WaveformStyle)` alongside the existing `RealOverlay::spawn()`. Both spawn the same `run_event_loop`; `spawn()` passes `WaveformStyle::Bars` as a placeholder (it is used only when `OverlayState::Recording` is active in a standalone context; the interactive path never enters the standalone visualisation branch).

### Phase 5 — Standalone Visualisation Rendering

All three rendering functions are called from `redraw()` when `OverlayState::Recording { .. }` is active and the overlay was spawned via `spawn_waveform`. They replace the transcript text block for this state only; the status label (`"RECORDING"`) and accent stripe are still drawn first.

The content area is defined as:
- `x0 = (PADDING_X + ACCENT_WIDTH) * scale`, `x1 = w as f32 - PADDING_X * scale`
- `y_top` = immediately below the status label baseline (same position text rows would start)
- `y_bot = h as f32 - PADDING_BOT * scale`

#### Bars style

- [ ] 5.1. Add `draw_waveform_bars(buf, stride, h, levels, x0, x1, y_top, y_bot, accent, scale)` free function.
  - `n = levels.len()`, skip if 0.
  - `bar_w = (x1 - x0) / n as f32 - 1.0 * scale` (1 px gap between bars).
  - For each bar `i`, `bar_h = levels[i] * (y_bot - y_top)`. Draw a filled rounded rect (`fill_round_rect`, radius `2.0 * scale`) from `(x0 + i * slot_w, y_bot - bar_h)` to `(x0 + i * slot_w + bar_w, y_bot)`.
  - Colour: the accent colour with alpha linearly interpolated between `0x33` (silence) and `0xFF` (full), so bars glow brighter as volume rises.
  - Draw a 1-physical-pixel horizontal floor line at `y_bot` in `COLOR_TEXT_DIM` so the baseline is visible during silence.

#### Oscilloscope style

- [ ] 5.2. Add a `draw_line_segment(buf, stride, w, h, x0, y0, x1, y1, color)` pixel helper using Bresenham's line algorithm. The helper clips to the buffer bounds and blends using the existing `blend()` function with full alpha (255).
- [ ] 5.3. Add `draw_oscilloscope(buf, stride, h, osc_samples, x0, x1, y_top, y_bot, accent, scale)` free function.
  - `visible = min(osc_samples.len(), display_samples)` where `display_samples = (x1 - x0) as usize` (one sample per pixel column, subsampled if more samples than pixels).
  - Centre line `y_mid = (y_top + y_bot) / 2.0`.
  - For each pixel column `px` in `[x0, x1)`: map to sample index, get amplitude (last `visible` samples of the ring buffer, newest on right). `y = y_mid - amplitude * (y_bot - y_top) / 2.0`, clamped to `[y_top, y_bot]`.
  - Connect consecutive (px, y) pairs with `draw_line_segment` in accent colour at full alpha. Anti-aliased by drawing a second pass at 50 % alpha ±1 pixel vertically.
  - Draw the centre line at `y_mid` in `COLOR_TEXT_DIM` alpha `0x22` as a subtle guide.

#### Pulse style

- [ ] 5.4. Add `draw_pulse(buf, stride, h, level, x0, x1, y_top, y_bot, accent, scale)` free function.
  - The pulse is a filled circle centred in the content area.
  - `cx = (x0 + x1) / 2.0`, `cy = (y_top + y_bot) / 2.0`.
  - `max_r = min((x1 - x0), (y_bot - y_top)) / 2.0 * 0.85`.
  - `min_r = max_r * 0.25` (always visible even at silence).
  - `r = min_r + level * (max_r - min_r)`.
  - Draw an outer halo ring: same centre, `r_halo = r * 1.35`, accent colour at alpha `0x22`. Draw with `fill_round_rect` using `r_halo` as corner radius on a square `(cx-r_halo, cy-r_halo, cx+r_halo, cy+r_halo)`.
  - Draw the inner filled circle: accent colour at alpha linearly interpolated between `0x88` (silence) and `0xFF` (loud). Draw with `fill_round_rect` using `r` as corner radius.

### Phase 6 — Right-Side VU Bar for the Interactive Overlay

- [ ] 6.1. Add layout constants: `VU_BAR_WIDTH: f32 = 8.0` and `VU_BAR_GAP: f32 = 6.0` (gap between text area and bar). These are logical pixels; multiply by `scale` in the renderer.
- [ ] 6.2. Modify the text wrap computation in `App::about_to_wait` (`real.rs:573`): when `config.overlay.volume_bar` is true and the state is `LiveDictating`, subtract `(VU_BAR_WIDTH + VU_BAR_GAP) * scale` from the `max_w` used in `wrap_text`. This prevents the VU bar from overlapping the transcript. The wrap width modification applies only for the `LiveDictating` state; `Processing` and `Recording` states are unchanged.
- [ ] 6.3. Add `draw_vu_bar(buf, stride, h, level, x_right, y_top, y_bot, accent, scale)` free function.
  - Bar occupies `(x_right - VU_BAR_WIDTH * scale, y_top)` to `(x_right, y_bot)`.
  - Fill from the bottom: filled height = `level * (y_bot - y_top)`, drawn in accent colour at full alpha (`0xFF`).
  - Unfilled portion above: accent colour at alpha `0x22` (ghost track so the bar bounds are always visible).
  - Rounded caps: `fill_round_rect` with `radius = VU_BAR_WIDTH * scale / 2.0` for the full ghost track, then again for the filled portion.
  - No label; the visual position is self-explanatory in context.
- [ ] 6.4. In `redraw()`, after drawing the accent stripe and before drawing text, check: if `app.state == OverlayState::LiveDictating` and `volume_bar` config flag is set and `!app.levels.is_empty()`: compute `x_right = w as f32 - PADDING_X * scale`, call `draw_vu_bar()` using the most recent entry in `app.levels` as the current level.

### Phase 7 — Decoupling Overlay Spawn from `interactive`

- [ ] 7.1. In `crates/fono/src/session.rs`, change every `#[cfg(feature = "interactive")]` guard covering the `overlay` field, the `with_overlay` builder method, and the spawn block to `#[cfg(any(feature = "interactive", feature = "waveform"))]`.
- [ ] 7.2. Extend the overlay spawn block (`session.rs:323-340`):
  - Existing branch: `if config.interactive.enabled` → `RealOverlay::spawn()` (unchanged).
  - New branch: `else if cfg!(feature = "waveform") && config.overlay.waveform` → `RealOverlay::spawn_waveform(config.overlay.style)`.
  - Failure path for both branches is identical: log a `warn!` and continue without overlay.
- [ ] 7.3. In `spawn_pipeline()`, clone the overlay handle from `self.overlay` (same pattern as cloning `self.stt`, `self.history`, etc.). At pipeline completion — success, empty-transcript, or error — call `overlay.set_state(OverlayState::Hidden)`.

### Phase 8 — Level Ticker in the Batch Recording Path

- [ ] 8.1. Add `level_task: Option<tokio::task::AbortHandle>` to the `CaptureSession` struct.
- [ ] 8.2. At the end of `on_start_recording()`, after the `CaptureSession` is stored: check whether an overlay handle is present (cfg gate active). If so, clone `Arc<Mutex<RecordingBuffer>>` and the `OverlayHandle`. Determine the configured style from `self.current_config().overlay.style`.
  - For `Bars` and `Pulse` styles: spawn a tokio task that loops at `tokio::time::sleep(33ms)`. Each tick: lock the buffer, iterate the last 800 samples (50 ms at 16 kHz), compute RMS without collecting (iterate the slice directly while lock is held), release the lock, normalise against `WAVEFORM_RMS_CEILING: f32 = 0.04` (named constant), call `overlay.push_level(normalised)`. Store `AbortHandle` in `CaptureSession.level_task`.
  - For `Oscilloscope` style: spawn a tokio task that loops at `tokio::time::sleep(16ms)` (≈ 60 fps). Each tick: lock the buffer, snapshot the last 320 samples (20 ms at 16 kHz), release the lock, call `overlay.push_samples(snapshot)`. Store `AbortHandle` in `CaptureSession.level_task`. Additionally send an `AudioLevel` every third tick for the floor-line update.
- [ ] 8.3. Set `overlay.set_state(OverlayState::Recording { db: 0 })` immediately after the level task is spawned to make the panel visible.
- [ ] 8.4. In `on_stop_recording()`, before calling `stop_and_drain()`: abort the level task handle, then call `overlay.set_state(OverlayState::Processing)` so the panel shifts to amber while STT runs.
- [ ] 8.5. In `on_cancel()`: abort the level task and call `overlay.set_state(OverlayState::Hidden)` immediately (no pipeline phase).

### Phase 9 — Level Tap in the Live Dictation Path (VU Bar Feed)

- [ ] 9.1. In the drain task at `session.rs:893-901`, add a side-channel alongside `pump.push(&chunk)`: if an overlay handle is present (cfg gate) and `config.overlay.volume_bar` is true, compute RMS of `chunk` inline (the chunk is already 16 kHz mono f32), normalise against `WAVEFORM_RMS_CEILING`, call `overlay.push_level(normalised)`. This is the only modification to the drain task; it does not affect the pump or broadcast channel.
- [ ] 9.2. The overlay handle for the live dictation path is already stored in `LiveCaptureSession.overlay` (`session.rs:914`). Clone it into the drain task's async closure (the same way other handles are moved in).

---

## Verification Criteria

- With `[overlay] waveform = true`, `style = "bars"`, `[interactive] enabled = false`: a 640-wide transparent panel appears bottom-centre on hotkey press, animates scrolling amplitude bars during speech, transitions to amber ("POLISHING") when STT runs, and hides on completion.
- Switching `style` to `"oscilloscope"` shows a connected-line waveform; switching to `"pulse"` shows a breathing circle; accent colours and status label remain correct in all three modes.
- With `[interactive] enabled = true` and `[overlay] volume_bar = true`: the existing interactive overlay panel shows a narrow vertical VU bar on the right side during `LiveDictating`; the transcript text reflows to avoid the bar; the bar is absent during `Processing` and `Hidden` states.
- With `[overlay] volume_bar = false`: interactive overlay behaviour is identical to the current baseline; no bar is drawn and the text wrap width is unchanged.
- Bars respond proportionally: sustained loud speech fills bars/pulse to near-full; sustained silence renders near-zero bars with the floor line visible.
- Panel does not steal focus from the active window (existing `with_active(false)` + `with_override_redirect(true)` invariants maintained).
- Slim build (`--no-default-features --features tray`) compiles without error; `waveform` feature is absent from that flag set.
- `cargo clippy --all-features` and `cargo test --all-features` produce no new warnings or failures.

---

## Potential Risks and Mitigations

1. **Oscilloscope snapshot allocation in the ticker task**
   The ticker copies up to 320 `f32` values per tick (~16 ms). At 60 fps this is ~1.2 MB/s of heap allocation.
   Mitigation: Pre-allocate a fixed-size `[f32; 320]` array on the stack in the task, fill it from the buffer slice, and send it as a `Vec` only when the overlay handle is `Some`. Alternatively, size the snapshot to the actual buffer tail length (often shorter than 320 during short recordings) to reduce average allocation.

2. **Oscilloscope line rasterisation at high DPI**
   On 2× HiDPI displays `scale = 2.0`, so the oscilloscope's column-per-pixel mapping produces half the visual columns per logical pixel. The Bresenham helper connects pairs so the waveform remains continuous, but lines are 1 physical pixel wide (thin).
   Mitigation: Draw each segment twice: once at the computed position and once at `y ± 1` physical pixel with 50 % alpha. This gives a 2-physical-pixel stroke without full anti-aliasing complexity. Document as "good enough for v1; full MSAA deferred".

3. **VU bar overlap with long transcript lines**
   If `wrap_text` reflows based on the modified max width but `redraw` draws text at the original padding, the rightmost characters may still underlap the bar on the first render before `UpdateText` triggers a re-wrap.
   Mitigation: The wrap computation is triggered in `about_to_wait` when `AudioLevel` is first received (which sets `needs_redraw`). On the first `LiveDictating` render before any audio level arrives, the bar is not drawn (`app.levels.is_empty()` guard). The bar appears only once `push_level` is called, by which point the wrap width is already adjusted.

4. **`WAVEFORM_RMS_CEILING` tuning**
   `0.04` RMS may suit near-field cardioid microphones but clip frequently for gain-boosted or close-talk setups.
   Mitigation: Define as `const WAVEFORM_RMS_CEILING: f32 = 0.04` in `session.rs` with an explanatory comment. A follow-up `[overlay].rms_ceiling` config knob can be wired once real-world data is collected.

5. **Drain task captures overlay handle but not config**
   The drain task is a `tokio::spawn` closure that moves owned values. It needs both the overlay handle and the `volume_bar` flag at spawn time.
   Mitigation: Capture `volume_bar: bool` (a `Copy` primitive) and `Option<OverlayHandle>` (cheap `Arc` clone) into the closure. No config lock is held in the hot path; the boolean is snapshot at session start, consistent with how `grace_ms` and other knobs are read.

---

## Alternative Approaches

1. **Broadcast channel subscription for live dictation levels (instead of drain task tap)**: Subscribe to `AudioFrameStream` and compute RMS from `FrameEvent::Voiced { pcm }` events on a dedicated task. This is more idiomatic but adds a subscriber to the broadcast channel and a second async task just to forward one `f32`. The drain task tap is simpler and has identical data access, so it is preferred.

2. **Shared atomic for level instead of `OverlayCmd::AudioLevel`**: Use `Arc<AtomicU32>` (bit-cast f32) polled on a winit timer. Avoids channel overhead but couples `fono-overlay` to `fono-audio`/`session` at the type level and requires `ControlFlow::WaitUntil` in the event loop, firing redraws even when hidden. Rejected; the command-push model is consistent with existing overlay architecture.

3. **VU bar as a configurable visualisation style rather than an always-on overlay feature**: Put the VU bar under `[overlay].style` and make it one of the selectable modes for the interactive overlay too. This unifies the two features under one config knob but conflates the standalone "what kind of visualisation to show" choice with the interactive "add signal feedback to existing text UI" choice. Kept separate per the user's feedback framing.

4. **Separate `WaveformStyle` for interactive vs. standalone**: Allow independent style selection via `[overlay].style` (standalone) and `[overlay].interactive_style` (live dictation). Deferred; the VU bar is the only sensible interactive-mode visualisation since text occupies most of the panel, and adding a full oscilloscope/bar chart alongside transcript text would compete for vertical space.
