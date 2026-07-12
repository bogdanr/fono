# Brain Visualization — Three Concept Mockups (decision aid)

## Objective

Produce **static PNG mockups of three candidate visualization concepts** so the user
can look at them side by side and pick a direction, *before* investing in a full
redesign. This is a throwaway exploration step: it must not disturb the production
scene. Deliverable = image files the user can open, plus a short note on which frames
show what.

Concepts to mock (all three):
1. **Layer Bars** — music-equalizer style: each vertical bar = one transformer layer,
   left→right. Height/brightness = layer activation. A bright spark sweeps left→right
   for each generated word (decode); a single wave fills all bars at once for prefill.
   Bars bounce to sound while listening/speaking. Dense = solid bars; MoE = each bar is
   a stack of expert cells, only a few lit, warm = amber (RAM), cold = blue (on-disk).
2. **Neural Current** — flow field of oriented glowing filaments over a left→right layer
   spine. Filament length = activation magnitude, angle = flow toward next-firing region,
   brightness = intensity. Decode = a pulse travelling left→right; prefill = a broad
   tide sweeping once; MoE = filaments clumping into expert cells (warm/cold tint).
3. **Deep Scan** — telemetry river: time scrolls right→left, vertical position = layer
   depth (bottom = first layer, top = last). Decode = each token emits a diagonal
   cascade climbing bottom→top then scrolling left; prefill = a full-height flare across
   all rows at once. Dense = solid rows; MoE = sparse lit cells within rows (warm/cold).

## Context / what already exists (reuse, do not rebuild)

- Offline render harness: `crates/fono-overlay/examples/cortex_gallery.rs` (renders phase
  frames to PPM; convert to PNG with `magick`/`convert`). Extend or copy it.
- CPU software renderer + glow accumulator: `crates/fono-overlay/src/r3d.rs`,
  `crates/fono-overlay/src/renderer.rs`.
- Current (rejected) scene lives in `crates/fono-overlay/src/cortex.rs` — do **not**
  edit it for mockups; the mockups are standalone throwaway draw code.
- Real panel geometry (measured live): a wide, short strip ~**810 × 96 px** with a dark
  translucent rounded background and a green state-accent bar on the left. A taller
  variant up to ~640 × 240 also exists.

## Implementation Plan

- [ ] Task 1. Save current work to a throwaway branch. Create a branch (e.g.
  `wip/brain-viz-old-scene`) capturing the present `cortex.rs` scene so nothing is lost,
  then continue mockup work on a fresh exploration branch (e.g. `explore/brain-mockups`).
  Rationale: user explicitly asked to shelve the current design and start over safely.

- [ ] Task 2. Add a standalone throwaway mockup example (e.g.
  `crates/fono-overlay/examples/brain_mockups.rs`) that renders each concept with its own
  self-contained draw function. Keep it isolated from `cortex.rs` so production code is
  untouched and the example can be deleted later. No new dependencies.
  Rationale: mockups are exploratory; isolation keeps the decision cheap and reversible.

- [ ] Task 3. Drive all mockups from a single **deterministic synthetic signal set**
  (fixed seed): per-layer activation profile, a per-token entropy value, an MoE routing
  pattern (which experts fire per layer), and residency (warm/cold) flags. Use a
  representative layer count (~40) and expert count (~64–128) so both dense and MoE read
  correctly. Rationale: deterministic, representative frames make the concepts comparable
  and reproducible without needing a live model.

- [ ] Task 4. For **each of the three concepts**, render key frames at the real panel
  size (~810 × 96), saved as PNG:
  - `listening` (sound-reactive, mic-spectrum driven)
  - `thinking_prefill` (the prefill animation mid-sweep)
  - `speaking_decode_dense` (a dense model mid-word)
  - `speaking_decode_moe` (an MoE model mid-word, showing lit experts + warm/cold)
  Optionally also render each at the taller ~640 × 240 variant.
  Rationale: these four frames cover every objective (sound reactivity, prefill vs decode
  distinction, dense vs MoE distinction) and let the user judge legibility at true size.

- [ ] Task 5. Render on a dark translucent background with the left state-accent bar, and
  use state-appropriate accent colours (matching the tray state palette / ADR 0013) so
  the mockups read as the real product, not a lab plot. Rationale: the decision must be
  made on how it will actually look in the overlay.

- [ ] Task 6. Collect outputs into one directory (e.g. `/tmp/brain_mockups/`), convert
  PPM→PNG, and present the user a short index: filename → concept + phase. Rationale: the
  whole point is for the user to open and compare them quickly.

## Verification Criteria

- Three concepts × at least four phase frames each are produced as openable PNGs at
  ~810 × 96.
- In every concept: prefill and decode frames are visually distinct at a glance; dense
  and MoE decode frames are visually distinct; warm vs cold experts are distinguishable
  in the MoE frames.
- The listening frame visibly responds to a (synthetic) sound spectrum.
- Production `cortex.rs` and the live overlay behaviour are unchanged; all mockup code is
  confined to the throwaway example on the exploration branch.
- No new crate dependencies added (size budget untouched); `cargo fmt` / `clippy` clean
  for the new example.

## Potential Risks and Mitigations

1. **Mockups look busy/noisy at 810×96 (the failure mode of the previous design).**
   Mitigation: favour few, bright, high-contrast elements on a dark field; render at true
   size from the start; if a concept is illegible small, note it explicitly as a finding.
2. **Synthetic data makes a concept look better/worse than real data would.**
   Mitigation: use a plausible activation profile (rising norms with depth, sparse MoE
   routing); optionally dump one real activation trace via the existing capture tap to
   sanity-check, but do not block the mockups on it.
3. **Scope creep into a full implementation.** Mitigation: this step ends at static PNGs;
   no phase machine, replay, or backend wiring. The full redesign plan comes after the
   user picks a concept.

## Alternative Approaches

1. **Hybrid mockup** (Deep Scan base + Neural Current filaments on the live edge) — render
   as an optional fourth frame if the two feel complementary.
2. **Animated GIF instead of static PNG** — more faithful to motion but slower to iterate;
   defer unless a static frame can't convey a concept (e.g. the decode sweep).

## Handoff note

Planning/Muse mode is read-only and cannot run the renderer or write code. Switch to the
implementation agent (Forge) to execute this spec. After the user picks a concept from the
mockups, return here to write the full redesign plan (signal-provider abstraction, phase
machine reuse, replay sync, backend wiring, budgets).
