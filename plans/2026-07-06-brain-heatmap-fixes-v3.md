# Brain Visualization — Heatmap Fixes, Phase Redesign, Audio↔Weight Synergy & MoE Constellation

## Objective

Resolve the seven issues raised against the shipped Activation Heatmap (`cortex`)
style on `explore/brain-mockups`, add the user's audio↔weight synergy mechanism, and
add a dedicated **MoE Constellation** representation for mixture-of-experts models.

Turn the heatmap from a low-contrast, buried, uniformly lit grid into a clean,
high-contrast square lattice that: reads clearly at 810×96, never occludes its own
status label, shows non-activating cells as genuinely OFF, distinguishes prefill from
decode with different motion, and pulses with the spoken/heard voice — where each
frequency band lights its corresponding row as a subtle, clearly-visible overlay that
never dominates the decode animation. All grounded in real signals.

For MoE models, replace any "one cell per expert" scheme with a **Constellation**: a
sparse field of dim expert-stars where only the routed top-k ignite per token (brightness
= routing weight), with faint threads tracing the chosen set — expressing the *idea* of
experts (many dormant, a few firing, the set changing per word) beautifully rather than
enumerating all of them.

Scope: the heatmap scene (`crates/fono-overlay/src/cortex.rs::draw_cortex`), the
renderer draw-order (`renderer.rs`), the phase machine's prefill-vs-decode distinction,
one new real signal (TTS output amplitude, as a few FFT bands) in the
`fono-core → overlay` path, and a new MoE constellation draw path fed by the real
routing tensors already captured. Iterate offline in the gallery harness
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

## MoE data availability (grounded in the capture path)

- **Activated experts — REAL and targetable now.** The capture tap reads the real router
  tensors `ffn_moe_topk-<layer>` (chosen expert ids, top-k order) and
  `ffn_moe_weights-<layer>` (routing weights) per layer per token
  (`brain_tap.rs:32-33, 424-442, 571-583`). They are tiny (dozens of bytes/layer), well
  within the <1% budget, and already flow into `BrainKeyframe.experts` →
  `cortex.rs` `ingest` (sets `moe = true`, fills `routing[layer]`, `:390-401`).
  Prerequisites to make it real: (a) an actual MoE GGUF loaded; (b) embedded backend with
  `brain_capture = true`; (c) validate the tensor names against what llama.cpp 0.1.150
  emits for the specific MoE arch (exact-match is fragile, `brain_tap.rs:431-432`).
- **Faithfulness gaps to close (cheap):** the current renderer ignores the captured
  **weights** for intensity and mixes a **synthetic hash roll** into which cells light
  (`cortex.rs:1184-1196`). The Constellation must ignite **purely the real top-k ids**
  with **brightness = real routing weight** — no hash roll.
- **Warm/cold residency — NOT real now (deferred).** `expert_warm(id)` is a stable hash
  of the id, explicitly a stand-in (`cortex.rs:1318-1322`). Real residency needs each
  expert tensor's mmap range exposed from llama (not available via `llama-cpp-2`/`-sys`
  today) + a `mincore()` scan per reply (Linux; Windows `QueryWorkingSetEx`), and only
  means anything when experts are actually offloaded (`--n-cpu-moe` / D4). Until that
  plumbing lands, warm/cold is either an **explicitly-labelled synthetic tint** or
  **omitted**. This plan keeps it as a clearly-flagged synthetic tint, gated to be
  trivially disable-able, and defers real residency to its own task tied to offload.

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

## MoE Constellation — chosen expert representation

Principle: never one cell per expert, never try to enumerate all of them. Express the
*idea* of experts — many dormant, a few firing, the set changing per token — beautifully.

- **Field of stars.** A sparse field of dim points across the panel, each a sampled
  expert position. The full population is only *implied* (a faint substrate); we do NOT
  attempt to show all of a 128- or 256-expert model. A tiny "k / N active" HUD readout
  (real counts from the topk tensor) conveys the true scale.
- **Ignition = real routing.** Per token, the **real top-k routed experts ignite** (from
  `ffn_moe_topk`), with **brightness/size = real routing weight** (`ffn_moe_weights`).
  No synthetic hash roll deciding which fire (that gap is closed here). The rest stay
  dark (true-OFF, per A3).
- **Threads.** Faint light-threads trace between the chosen experts (in top-k weight
  order) — draws a little constellation per token. Threads decay quickly.
- **Motion over time = the payoff.** As decode advances token-by-token (replay-synced to
  TTS), the ignited constellation **changes** — you watch the model light a different
  circuit for each word. A slow **heat-trace** accumulation shows which regions of expert
  space carried the whole reply.
- **Layer dimension.** Depth is expressed as *progression*: the constellation reflects the
  routing of the currently-active decode layer, updating as the pulse steps through
  layers (depth becomes time, not a competing spatial axis). Keeps the "watch it think"
  motion without a separate layer grid.
- **Warm/cold tint (deferred/synthetic).** If shown, an ignited star's hue leans warm
  (amber, RAM-resident) vs cold (blue, offloaded) — but per the MoE-availability section
  this is a **synthetic stand-in today**, clearly flagged, and trivially disable-able
  until real residency lands. Default: keep the ignition palette honest (weight-driven)
  and treat warm/cold as an optional decorative layer, not a truth claim.
- **Audio synergy still applies.** The global amplitude pulse and (optionally) a subtle
  spectral shimmer modulate star brightness during speaking/listening, same subtlety cap
  and honesty invariant as the heatmap.
- **Degraded/cloud.** With no routing data, ignite a plausible k-of-N per word to the TTS
  cadence (expressive, honestly labelled as activity — never claims specific experts),
  still pulsing to the real TTS audio from E1.

Relationship to the heatmap style: the Constellation is the **MoE presentation**; dense
models use the square heatmap. Both share plumbing (phase machine, replay clock, audio
signal, glow primitives). Whether MoE is a distinct selectable style or an
auto-switched mode of the cortex style is decided during the mockup round (default:
auto-switch to Constellation when `moe == true`, so the user need not pick).

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

### G. MoE Constellation (chosen expert representation)

- [ ] Task G1. **Faithful routing render.** Close the honesty gaps: ignite **only the
  real top-k ids** from `ffn_moe_topk` with **brightness/size = real `ffn_moe_weights`**;
  remove the synthetic hash roll (`cortex.rs:1184-1196`) from which experts light. Add
  the "k / N active" HUD readout from the real topk/expert counts.
- [ ] Task G2. **Constellation scene.** Draw the sparse star field: dim implied
  population, ignited routed experts (glow sprites, weight-driven), fast-decaying
  threads between the chosen set in weight order, and a slow heat-trace accumulation
  over the reply. Depth = progression: reflect the currently-active decode layer's
  routing, updating as the pulse steps through layers.
- [ ] Task G3. **Warm/cold as optional flagged synthetic tint.** Keep `expert_warm` as a
  clearly-labelled stand-in behind a switch that defaults to weight-honest palette;
  document that real residency is deferred (see G5). Never present the tint as a truth
  claim.
- [ ] Task G4. **Audio + degraded paths.** Apply the global amplitude pulse / subtle
  shimmer (E1) to star brightness under the same subtlety cap; and the cloud/degraded
  path (ignite plausible k-of-N per word to TTS cadence, honestly labelled activity).
- [ ] Task G5 (**deferred, separate**). **Real residency.** Expose expert tensor mmap
  ranges from llama, `mincore()` (Linux) / `QueryWorkingSetEx` (Windows) scan per reply,
  wire into the keyframe stream, and drive warm/cold from real memory state. Gated on the
  expert-offload (`--n-cpu-moe` / D4) feature. Out of scope for this pass; captured here
  so warm/cold has a real home later.

### F. Validation & gates

- [ ] Task F1. Iterate all phases in the offline gallery harness at the real strip size
  (810×96) — idle / listening / prefill / decode-dense / decode-MoE (Constellation) /
  speaking-with-audio — and self-review against: uniform gaps, square cells, visible OFF
  cells, readable label, obvious voice reactivity, distinct prefill vs decode, subtle-
  but-visible audio row-glow that does not dominate, and a sparse/mesmerizing MoE
  constellation whose ignited set visibly changes per token.
- [ ] Task F2. Run the pre-commit gate (`cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`) and build the daemon for a live demo.
- [ ] Task F3. Re-confirm budgets: capture <1% (E1 is cheap playback FFT, not per-token
  tensor reads; routing tensors are dozens of bytes/layer), frame within ~4 ms, zero new
  crates.

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
- For MoE models, the Constellation shows a sparse field where only the real routed
  top-k experts ignite (brightness = real weight), the set visibly changes per token,
  a "k / N active" readout conveys scale, and warm/cold (if shown) is clearly a synthetic
  stand-in, not presented as real memory state.
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
7. **Constellation dishonesty (synthetic hash roll / fake warm-cold read as truth).**
   Mitigation: G1 removes the hash roll and drives ignition purely from real top-k +
   weights; G3 keeps warm/cold as a clearly-flagged, default-off-ish synthetic tint until
   G5 lands real residency. HUD says "activity" not "expert N" in degraded mode.
8. **No MoE model available to validate G1 routing / tensor names.**
   Mitigation: validate `ffn_moe_topk`/`ffn_moe_weights` names on a real MoE GGUF before
   trusting the render; until then, drive the Constellation from the gallery's synthetic
   routing so the *look* can be locked, and gate the real path on name validation.

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
4. **MoE Constellation (chosen) vs expert tiles vs per-cell expert grid.**
   Per-cell grid can't scale past a handful of experts and looks ugly at 128+; 3×3 tiles
   only yield ~2 tile-rows at 6 cells tall and collide with the layer axis. The
   Constellation expresses the *idea* of experts (sparse ignition, changing per token)
   beautifully at any expert count and fits the wide strip. Chose Constellation; tiles
   held in reserve as a "console" alternative.
