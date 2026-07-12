# Brain Visualization — Activation Heatmap Fixes & Phase Redesign

## Objective

Resolve the seven issues raised against the shipped Activation Heatmap (`cortex`)
style on `explore/brain-mockups`, turning it from a low-contrast, buried, uniformly
lit grid into a clean, high-contrast square lattice that: reads clearly at 810×96,
never occludes its own status label, shows non-activating cells as genuinely OFF,
distinguishes prefill from decode with different motion, and pulses with the spoken
voice during playback (audio↔brain "synesthesia") — all grounded in real signals.

Scope is the heatmap scene (`crates/fono-overlay/src/cortex.rs::draw_cortex`), the
renderer draw-order / VU-bar gating (`crates/fono-overlay/src/renderer.rs`), the phase
machine's prefill-vs-decode distinction, and one new real signal (TTS output
amplitude) in the `fono-core → overlay` path. Iterate offline in the gallery harness
(`crates/fono-overlay/examples/brain_mockups.rs` / `cortex_gallery`) at the real strip
size before rebuilding the daemon.

## Root-cause findings (grounded in code)

- **#1 Green bar:** the renderer's mic **VU bar** (`draw_vu_bar`, `renderer.rs:514`;
  green `VOICED_TICK_COLOR` `:560`) is drawn whenever `state_has_vu_bar(state)` is
  true, and `AssistantRecording` ("ASSISTANT") qualifies (`:2255`) — it paints over
  the heatmap. The cortex scene *also* draws its own accent-green left mic bar
  (`cortex.rs:1279-1288`). Two redundant audio bars nobody asked for.
- **#2 Label buried:** status label is intentionally drawn *first* so waveform styles
  paint over it (`renderer.rs:1903`). A full-panel heatmap therefore hides it.
- **#3 Listening unclear:** listening cell value is `0.30 + 0.60*act` + jitter
  (`cortex.rs:1217-1226`) — a near-uniform warm-red wall with a lifted floor; the
  mapping from voice → visual is not intuitive and contrast is low.
- **#4 Uneven gaps / not square:** `cell_w = area_w/ncols`, `cell_h = area_h/nrows`
  (`cortex.rs:1137-1138`) give non-square cells, and per-column float→int rounding of
  `gap*0.5` edges (`:1169-1175`) yields 1–2 px gap variance.
- **#5 No true-off cells:** `heat_ramp` floors at `0x0A0A10` (never background), so
  every cell glows faintly; nothing reads as "did not activate."
- **#6 Thinking ambiguity:** `AssistantThinking` shows only the prefill fill-wave
  (`cortex.rs:1227-1238`); there is no distinct decode animation and no prefill→decode
  transition within the thinking window.
- **#7 No real speaking audio:** during thinking/speaking the overlay is fed
  **synthetic** FFT frames at 20 fps (`session.rs:1972-1985`, generators
  `:1815-2115`), not the real TTS playback. The replay clock syncs decode *timing* to
  speech but nothing modulates the grid by the actual spoken *amplitude*.

## Implementation Plan

### A. Grid geometry — clean square lattice (#4, #5)

- [ ] Task A1. Replace the fractional `cell_w/cell_h` layout with a **fixed square
  cell + uniform integer gap**. Choose a target cell size and gap (bigger gap, per the
  reference screenshot), compute `cols_fit = floor((W + gap) / (cell + gap))` and
  `rows_fit` likewise, integer-align every cell origin to a pixel grid so all gaps are
  identical. Rationale: eliminates the rounding-induced uneven gaps and yields the
  chunky square look the user asked for.
- [ ] Task A2. **Bin the model's layers into `cols_fit` columns** (average activation /
  aggregate routing of the layers mapped to each column) instead of assuming
  `ncols == layer_count`. Rationale: decouples the visual column count from the model's
  layer count so squares stay square regardless of a 30- vs 48-layer model.
- [ ] Task A3. Introduce an **activation threshold**: cells below it render as true
  panel background (OFF), not a lifted floor. Apply consistently across phases so
  non-activating parameters/experts are visibly dark. Rationale: satisfies #5 and makes
  the grid read as sparse and meaningful rather than a solid wall.

### B. Chrome fixes — bar removal & label legibility (#1, #2)

- [ ] Task B1. For the `cortex` style, **suppress the renderer VU bar** (gate
  `draw_vu_bar` off when `style == cortex`) and **remove the cortex internal left mic
  bar** (`cortex.rs:1279-1288`). The heatmap grid becomes the sole audio visual.
  Rationale: removes the unrequested/redundant green bars (#1).
- [ ] Task B2. For full-panel styles (cortex), **draw the status label last, on top of
  the grid**, with a legibility treatment: a soft text shadow plus a subtle localized
  darkening/scrim of the cells directly behind the label text. Rationale: label stays
  readable over any grid content (#2) without a dead reserved strip.

### C. Listening — intuitive, obviously voice-reactive (#3)

- [ ] Task C1. Redesign the **Listening** phase as a **spectrum lattice**: map columns
  to frequency bands (low→high, left→right) from the real mic FFT bins, and light cells
  bottom-up per column proportional to that band's energy, on a mostly-dark grid
  (threshold from A3). Use a high-contrast cool→hot ramp. Rationale: silence reads as
  dark, speech as a dancing bright profile — an unmistakable, intuitive "sound reacting
  to voice," fixing the low-contrast uniform wall.
- [ ] Task C2. Tune the ramp/gain in the offline gallery at 810×96 until quiet vs loud
  is obvious at a glance and idle silence is near-black. Rationale: the previous look
  failed specifically on contrast and glanceability.

### D. Thinking — distinct prefill then decode (#6)

- [ ] Task D1. Define two visually distinct sub-animations and document the intended
  reading: **Prefill** = a single fill-wave flooding all columns left→right (whole model
  ingesting the prompt); **Decode** = a single hot column travelling left→right,
  repeating once per generated token (producing one token at a time). Rationale: gives
  the two phases unmistakably different motion (flood vs. travelling pulse).
- [ ] Task D2. Drive the prefill→decode transition from **real events**: show prefill
  while prefill-progress events arrive, then switch to the decode animation as soon as
  the first token keyframe lands (even before audio playback). Reuse the existing
  prefill events and replay clock; keep the phase machine honest. Rationale: answers
  "what does thinking show" — it shows both, sequentially, from real signals.

### E. Speaking — decode + voice synesthesia (#7)

- [ ] Task E1. **Add a real TTS output-amplitude signal** to the `fono-core → overlay`
  path: tap the playback stream during `AssistantSpeaking` (RMS, ideally a few FFT
  bands) and push it to the overlay, replacing/augmenting the synthetic frames for the
  cortex style. Rationale: the current speaking animation is synthetic
  (`session.rs:1972-1985`); honest synesthesia requires the actual spoken audio. **This
  is new plumbing, not a render tweak — flag its cost.**
- [ ] Task E2. During Speaking, **keep the travelling decode column** (replay-synced to
  TTS timing) **and globally modulate** grid brightness/bloom by the live TTS amplitude
  from E1, so the whole lattice pulses with each spoken word while the decode column
  travels. Rationale: the "synesthesia" the user described — brain activity visibly
  breathing with the voice.
- [ ] Task E3. Preserve the honest **degraded/cadence** path for external backends
  (no capture, no internals): decode column fires to word cadence and the grid still
  pulses to TTS amplitude (E1 works regardless of backend), without inventing internal
  detail.

### F. Validation & gates

- [ ] Task F1. Iterate all phases in the offline gallery harness at the real strip size
  (810×96) — dump PNGs for idle / listening / prefill / decode-dense / decode-MoE /
  speaking-with-audio — and self-review against: uniform gaps, square cells, visible
  OFF cells, readable label, obvious voice reactivity, distinct prefill vs decode.
- [ ] Task F2. Run the pre-commit gate (`cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`) and build the daemon for a live demo.
- [ ] Task F3. Re-confirm budgets: capture stays <1% (E1's amplitude tap is cheap RMS,
  not per-token tensor reads), frame stays within the ~4 ms envelope, zero new crates.

## Verification Criteria

- No green (or any) VU/mic bar appears in the cortex style; the grid is the only audio
  visual.
- The status label is legible in every phase, including over the brightest grid cells.
- Cells are visibly square with uniform gaps; low-activation cells render fully OFF
  (background), so the grid looks sparse/meaningful, not a solid wall.
- Listening clearly and intuitively tracks the voice (dark in silence, a lively
  spectrum profile while speaking), high contrast.
- Thinking shows a prefill flood first, then a distinct travelling decode column —
  the two are unmistakably different motions and are driven by real events.
- During speaking, the decode column continues (synced to speech timing) and the grid
  pulses with the real TTS amplitude; the effect degrades honestly for cloud backends.
- All pre-commit gates green; capture <1%, frame <~4 ms, no new dependency.

## Potential Risks and Mitigations

1. **Layer→column binning hides per-layer detail.**
   Mitigation: pick `cell` size so `cols_fit` is close to typical layer counts
   (~30–48); when fewer columns, average adjacent layers — the visual reads flow, not
   exact per-layer values (acceptable for a viz, keeps it grounded).
2. **TTS amplitude tap (E1) adds real plumbing and could regret budget/complexity.**
   Mitigation: cheap RMS on the already-decoded playback buffer (no model work); push
   at the existing 20 fps overlay cadence; feature-gate to the cortex style so other
   styles are unaffected.
3. **Removing the VU bar for cortex may lose info some users relied on.**
   Mitigation: the heatmap's listening spectrum replaces it with a richer read; scope
   the suppression strictly to the cortex style so other styles keep the VU bar.
4. **True-OFF cells (A3) could make the grid look empty at low activity.**
   Mitigation: tune the threshold and keep a faint (but clearly sub-active) hint for
   near-threshold cells; validate in the gallery across quiet and busy frames.
5. **Prefill→decode transition may flicker if events are bursty.**
   Mitigation: latch the decode state once the first token keyframe arrives; smooth the
   fill-wave→column handoff over a short cross-fade.

## Alternative Approaches

1. **Listening as bottom-up spectrum (chosen) vs. per-cell amplitude noise.**
   The spectrum reading is intuitive and obviously voice-linked; per-cell noise is
   prettier but ambiguous. Chose intuitiveness per the user's explicit ask.
2. **Speaking synesthesia via real TTS amplitude (chosen) vs. keeping synthetic frames.**
   Synthetic is zero-plumbing but dishonest and doesn't actually track the voice; the
   user explicitly wants reaction to the spoken sound, so the real tap is warranted.
3. **Fixed square cells with layer binning (chosen) vs. stretching one cell per layer.**
   Stretching keeps exact per-layer mapping but reintroduces non-square cells and uneven
   gaps — the exact defects being fixed. Chose square + binning.
