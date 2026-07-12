# Brain Visualization — Activation Heatmap Fixes, Phase Redesign & Audio↔Weight Synergy

## Objective

Resolve the seven issues raised against the shipped Activation Heatmap (`cortex`)
style on `explore/brain-mockups`, and add the user's audio↔weight synergy mechanism.
Turn the style from a low-contrast, buried, uniformly lit grid into a clean,
high-contrast square lattice that: reads clearly at 810×96, never occludes its own
status label, shows non-activating cells as genuinely OFF, distinguishes prefill from
decode with different motion, and pulses with the spoken/heard voice — where each
frequency band lights its corresponding row as a subtle, clearly-visible overlay that
never dominates the decode animation. All grounded in real signals.

Scope: the heatmap scene (`crates/fono-overlay/src/cortex.rs::draw_cortex`), the
renderer draw-order (`renderer.rs`), the phase machine's prefill-vs-decode
distinction, and one new real signal (TTS output amplitude, as a few FFT bands) in the
`fono-core → overlay` path. Iterate offline in the gallery harness
(`crates/fono-overlay/examples/brain_mockups.rs` / `cortex_gallery`) at the real strip
size before rebuilding the daemon.

## Two-bar clarification (critical — do not conflate)

There are TWO distinct left/right vertical bars. Only one is removed.

- **KEEP — VAD debug meter (`volume_bar`):** `draw_vu_bar` / `draw_vu_bar_advanced`
  (`renderer.rs:514`, `:584`), drawn at `:2189-2210` in the **right margin**
  (`x_right = w`). Gated by `config.overlay.volume_bar` (Off by default; user sets
  Simple/Advanced). Fed by real capture RMS via `push_level`. The Advanced flavour
  overlays green voiced ticks (`VOICED_TICK_COLOR`, `:560`) and amber silence ticks
  (`:561`) — this is the VAD-testing tool. **This code path must not be touched or
  gated off; it stays governed solely by the config.** The heatmap draws inside the
  waveform area and stops before the right margin, so it does not overlap this bar.
- **REMOVE — cortex-internal listening mic bar:** `cortex.rs:1279-1288`, drawn on the
  **left edge** of the grid (`x0`, ~3.5 px) every `Phase::Listening` frame, colored by
  the state accent (green for `AssistantRecording`), not config-controlled. This is the
  green bar the user flagged as unwanted; the redesigned listening spectrum + audio
  row-glow replaces its role.

## Root-cause findings (grounded in code)

- **#1 Green bar:** it is the cortex-internal left mic bar (`cortex.rs:1279-1288`),
  NOT the config VU meter. See the two-bar clarification above.
- **#2 Label buried:** status label is intentionally drawn *first* so waveform styles
  paint over it (`renderer.rs:1903`). A full-panel heatmap therefore hides it.
- **#3 Listening unclear:** listening cell value is `0.30 + 0.60*act` + jitter
  (`cortex.rs:1217-1226`) — a near-uniform warm-red wall; the voice→visual mapping is
  not intuitive and contrast is low.
- **#4 Uneven gaps / not square:** non-square `cell_w/cell_h` (`cortex.rs:1137-1138`)
  plus per-column float→int rounding of `gap*0.5` edges (`:1169-1175`) → 1–2 px gap
  variance.
- **#5 No true-off cells:** `heat_ramp` floors at `0x0A0A10` (never background), so
  every cell glows faintly; nothing reads as "did not activate."
- **#6 Thinking ambiguity:** `AssistantThinking` shows only the prefill fill-wave
  (`cortex.rs:1227-1238`); there is no distinct decode animation nor a prefill→decode
  transition within the thinking window.
- **#7 No real speaking audio:** during thinking/speaking the overlay is fed
  **synthetic** FFT frames at 20 fps (`session.rs:1972-1985`, generators `:1815-2115`),
  not the real TTS playback. The replay clock syncs decode *timing* to speech but
  nothing modulates the grid by the actual spoken *amplitude/spectrum*.

## Audio↔weight synergy — agreed design

Orthogonal axes so sound and computation never fight:

- **Columns = layers (depth).** The decode pulse travels left→right along this axis,
  one hop per token. This remains the primary motion.
- **Rows = frequency bands.** The live audio lights **rows**: each band brightens its
  row across the grid; per-band energy sets the glow strength. The brightest cell falls
  naturally where a loud band crosses the active decode column — emergent synesthesia.
- **Amplitude → a gentle global brightness pulse** on top of the per-band row glow, so
  both "frequency" and "amplitude" are represented.
- **Subtlety constraint:** cap the audio's additive contribution at ~25–30% of full
  intensity and give it a distinct shimmer/pulse feel (not a hard fill), so it reads as
  "sound" vs "activation" and never dominates the decode/prefill animation.
- **Honesty invariant:** the cell that glows shows a **real** activation/weight value;
  the audio only *modulates* its brightness. Which row a frequency maps to is a
  presentation choice, NOT a claimed causal link — never imply "this frequency drives
  this unit." Real substrate, decorative modulation.
- **Consistency:** frequency → rows in BOTH listening and speaking, so rows always mean
  frequency and columns always mean depth. Trade-off: with few grid rows the spectrum is
  coarse; acceptable for a glanceable bar, with the option to raise the cortex row count
  for finer bands.

## Implementation Plan

### A. Grid geometry — clean square lattice (#4, #5)

- [ ] Task A1. Replace fractional `cell_w/cell_h` with a **fixed square cell + uniform
  integer gap** (bigger gap per the reference). Compute `cols_fit`/`rows_fit` from the
  panel size, pixel-align every cell origin so all gaps are identical.
- [ ] Task A2. **Bin the model's layers into `cols_fit` columns** (average activation /
  aggregate routing of the layers mapped to each column) rather than assuming
  `ncols == layer_count`, so squares stay square regardless of layer count.
- [ ] Task A3. Introduce an **activation threshold**: cells below it render as true
  panel background (OFF), not a lifted floor. Apply consistently across phases so
  non-activating params/experts read as dark and the grid looks sparse/meaningful.

### B. Chrome fixes — bar removal & label legibility (#1, #2)

- [ ] Task B1. Remove ONLY the **cortex-internal listening mic bar**
  (`cortex.rs:1279-1288`). **Do NOT touch** `draw_vu_bar` / `draw_vu_bar_advanced` or
  the `config.overlay.volume_bar` gating — that is the VAD debug meter and must keep
  working (right margin, green voiced / amber silence ticks). The redesigned listening
  spectrum + audio row-glow replaces the internal bar's role.
- [ ] Task B2. For full-panel styles (cortex), **draw the status label last, on top of
  the grid**, with a soft text shadow plus a subtle localized darkening/scrim of the
  cells directly behind the label text, so it is legible over any grid content.

### C. Listening — intuitive, voice-reactive spectrum (#3)

- [ ] Task C1. Redesign **Listening** as a **spectrum lattice on the agreed axes**:
  **rows = frequency bands** (low→high) from the real mic FFT bins; each band lights its
  row proportional to energy, on a mostly-dark grid (threshold from A3), high-contrast
  cool→hot ramp. Silence = dark, speech = a lively dancing row profile — unmistakably
  reactive and intuitive. (Same rows-as-frequency mapping used in Speaking, per the
  synergy design.)
- [ ] Task C2. Tune ramp/gain in the offline gallery at 810×96 until quiet vs loud is
  obvious at a glance and idle silence is near-black.

### D. Thinking — distinct prefill then decode (#6)

- [ ] Task D1. Define two visually distinct sub-animations: **Prefill** = a single
  fill-wave flooding all columns left→right (whole model ingesting the prompt);
  **Decode** = a single hot column travelling left→right, repeating once per generated
  token. Distinct motion: flood vs. travelling pulse.
- [ ] Task D2. Drive the prefill→decode transition from **real events**: show prefill
  while prefill-progress events arrive, then switch to decode as soon as the first token
  keyframe lands (even before audio playback). Reuse the existing prefill events and
  replay clock; latch decode once the first keyframe arrives and cross-fade the handoff.

### E. Speaking — decode + voice synesthesia (#7 + synergy)

- [ ] Task E1. **Add a real TTS output signal** to the `fono-core → overlay` path: tap
  the playback stream during `AssistantSpeaking` and push **a few FFT bands + overall
  amplitude** (not just a single RMS) to the overlay, replacing the synthetic frames for
  the cortex style. Cheap FFT on the already-decoded playback buffer — no model work.
  **This is new plumbing, not a render tweak — flag its cost.**
- [ ] Task E2. During Speaking, render the **orthogonal synergy**: the travelling decode
  column (columns, replay-synced to TTS timing) **plus** per-row band glow (rows, from
  E1) capped at ~25–30% with a shimmer feel, **plus** a gentle global amplitude pulse.
  Decode stays primary; the sound decorates. Honesty invariant per the synergy design.
- [ ] Task E3. Preserve the honest **degraded/cadence** path for external backends
  (no capture/internals): decode column fires to word cadence and the grid still pulses
  and shimmers to the real TTS bands from E1 (works regardless of backend), without
  inventing internal detail.

### F. Validation & gates

- [ ] Task F1. Iterate all phases in the offline gallery harness at the real strip size
  (810×96) — idle / listening / prefill / decode-dense / decode-MoE / speaking-with-audio
  — and self-review against: uniform gaps, square cells, visible OFF cells, readable
  label, obvious voice reactivity, distinct prefill vs decode, subtle-but-visible
  audio row-glow that does not dominate.
- [ ] Task F2. Run the pre-commit gate (`cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`) and build the daemon for a live demo.
- [ ] Task F3. Re-confirm budgets: capture <1% (E1 is cheap playback FFT, not per-token
  tensor reads), frame within ~4 ms, zero new crates.

## Verification Criteria

- The unwanted left-edge green bar is gone; the config VU/VAD debug meter still renders
  (right margin, voiced/silence ticks) exactly as before when `volume_bar` is on.
- The status label is legible in every phase, including over the brightest grid cells.
- Cells are visibly square with uniform gaps; low-activation cells render fully OFF, so
  the grid looks sparse/meaningful, not a solid wall.
- Listening clearly and intuitively tracks the voice (dark in silence, a lively per-row
  spectrum while speaking), high contrast.
- Thinking shows a prefill flood first, then a distinct travelling decode column, driven
  by real events.
- During speaking, the decode column continues (synced to speech timing) and each
  frequency band lights its row as a clearly-visible but non-dominating overlay, with a
  global amplitude pulse; the effect degrades honestly for cloud backends.
- All pre-commit gates green; capture <1%, frame <~4 ms, no new dependency.

## Potential Risks and Mitigations

1. **Accidentally disabling the VAD debug meter.**
   Mitigation: Task B1 removes ONLY `cortex.rs:1279-1288`; the `draw_vu_bar*` /
   `volume_bar` path is explicitly out of scope. A test should assert the config VU bar
   still draws for `AssistantRecording` when `volume_bar` is on.
2. **Layer→column binning hides per-layer detail.**
   Mitigation: size cells so `cols_fit` is near typical layer counts (~30–48); average
   adjacent layers when fewer — the visual reads flow, not exact per-layer values.
3. **TTS FFT tap (E1) adds real plumbing / budget.**
   Mitigation: cheap FFT on the already-decoded playback buffer (no model work); push at
   the existing 20 fps cadence; feature-gate to the cortex style.
4. **Audio row-glow dominates or looks noisy.**
   Mitigation: hard cap at ~25–30%, shimmer (not fill), validate in the gallery on both
   quiet and loud frames; keep decode column visually primary.
5. **True-OFF cells make the grid look empty at low activity.**
   Mitigation: tune the threshold; keep a faint sub-active hint near threshold; validate
   across quiet and busy frames.
6. **Prefill→decode transition flicker.**
   Mitigation: latch decode on first token keyframe; short cross-fade on the handoff.

## Alternative Approaches

1. **Frequency → rows (chosen) vs frequency → columns.**
   Rows keep the sound orthogonal to the decode column (columns=depth), so they never
   fight and the mapping is consistent across listening/speaking. Columns would give a
   finer spectrum but collide with decode travel. Chose rows.
2. **Real TTS FFT (chosen) vs keeping synthetic speaking frames.**
   Synthetic is zero-plumbing but dishonest and doesn't track the voice; the user wants
   genuine reaction to the spoken sound, so the real tap is warranted.
3. **Fixed square cells + layer binning (chosen) vs one stretched cell per layer.**
   Stretching keeps exact per-layer mapping but reintroduces non-square cells and uneven
   gaps — the exact defects being fixed. Chose square + binning.
