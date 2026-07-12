# Brain Visualization — Thinking / Synthesizing / Speaking Redesign

## Objective

Replace the current Thinking/Speaking scene in `crates/fono-overlay/src/cortex.rs`
with a version that is genuinely legible, grounded in real data, and visually
coherent with the already-approved Listening spectrum and the System/360 style —
eliminating the floating "orb," the boring prefill sweep, and the flashing
prefill→decode handoff. This plan diagnoses each defect against the current
code before proposing the fix, per the instruction not to guess.

## Root causes (verified against current code, not assumed)

- [ ] **Confirmed:** the "orb" is a fixed-mid-height glow blob drawn every frame
      at `crates/fono-overlay/src/cortex.rs:1721-1725`, tracking only the
      horizontal decode position and completely detached from the grid cells —
      this is why it reads as a wired object floating independently of the
      visualization rather than as part of it.
- [ ] **Confirmed:** prefill has no row-level variation — `act`/`heat` are
      looked up once per column and reused for every row
      (`crates/fono-overlay/src/cortex.rs:1636-1638`), so each column renders
      as a flat vertical bar; combined with a single soft raised-cosine bump
      (`:702-713`), the whole phase is one boring blob sweeping left to right.
- [ ] **Confirmed:** the prefill→decode handoff flashes because (a) exactly one
      captured keyframe is consumed per **render tick**
      (`self.live_cursor += 1` at `:685-688`), which runs at the ~30 fps
      animation-pump rate with no relation to real token timing, and (b) the
      flood and decode fields are two independently-animated signals blended
      per-pixel every frame via a linear cross-fade
      (`:1660-1665`) — two uncoordinated animations mixed live is what produces
      the flashing/glitch look.
- [ ] **Confirmed:** there is no single deliberate motion per phase — flood,
      decode head, decode trail, jitter, heat, threshold cutoff, and the orb
      glow are five-plus additive terms stacked in the same formula
      (`:1659-1716`) with no unifying visual grammar, which is the structural
      reason nothing reads clearly regardless of which term gets tuned next.

## Design principles for the rewrite

- [ ] **One deliberate motion per phase.** Each phase gets exactly one primary,
      nameable animation. No phase mixes two independently-timed animations
      via continuous blending.
- [ ] **Rows always carry real, distinct meaning.** No phase may fill an entire
      column with one repeated value across all rows — that is what produces
      flat "blob" bars instead of a textured grid. (Listening already does
      this correctly with frequency-per-row; thinking/speaking must match.)
- [ ] **State transitions are discrete events, not continuous cross-fades of
      two live signals.** A phase change (prefill → decode) is a single,
      short, deterministic transition — e.g. a snap or a brief directional
      wipe — never an ongoing per-frame blend of two independently animated
      fields.
- [ ] **Decode is paced by real token/playback timing, not by render-tick
      rate.** Consuming "one keyframe per render tick" is banned; the decode
      clock must be driven by the actual token replay clock (already used
      elsewhere for TTS sync), decoupled from frames-per-second.
- [ ] **No untethered visual elements.** Every glow/bloom must be anchored to
      and sized against an actual grid cell or region it represents. No
      fixed-height, grid-independent shapes (the orb's defining flaw).
- [ ] **Palette stays consistent** with the already-approved Listening view and
      System/360 (`crates/fono-overlay/src/renderer.rs`) — same warm/cool
      language, no new competing hues introduced for these phases.

## Proposed concrete redesign

### Prefill — "the read sweep"
- [ ] Replace the soft raised-cosine bump with a **discrete, sequential
      column-ignition wipe**: columns latch to a real base-lit state one by
      one, left to right, paced by the real prefill-batch progress fraction
      (already published via `CortexCmd::Prefill`), not a synthetic timer.
- [ ] Within each column, give rows a **real per-row stagger/variation**
      (e.g. derived from the layer's sampled sub-activations or a stable
      per-row phase offset applied to the same sweep front) so the column
      fills with visible internal texture instead of appearing as a flat bar.
- [ ] Once a column is "read," it stays at a calm resting brightness (not
      zero, not full-hot) until the decode phase begins — this gives prefill
      a clear beginning-middle-end shape (progressively more of the grid lit)
      instead of one blob passing through and vanishing.

### Prefill → Decode handoff — "the snap"
- [ ] Replace the per-frame linear cross-fade with a **single short,
      deterministic transition** (e.g. a brief left-to-right wipe or a quick
      fade timed to a fixed short duration, not re-evaluated as a blend every
      tick) that fires exactly once when the first real token keyframe lands.
- [ ] After the transition completes, the prefill flood field is fully retired
      — decode rendering must not keep evaluating or blending against it.

### Decode (Thinking's second half, and Speaking) — "the token flare"
- [ ] Replace continuous per-tick keyframe consumption with a **replay clock
      driven by real token/playback timing** (reuse the existing TTS-sync
      replay clock mechanism already in the codebase for Speaking, and apply
      the same mechanism to the tail of Thinking/Synthesising) — never advance
      by "one frame per render tick."
- [ ] Each token becomes one clear discrete event: a column **flare** with
      real per-row structure driven by that token's actual per-layer sample
      data (not a uniform column fill), rising and decaying over a fixed
      short lifetime tied to the token's real timing slot, not to frame rate.
- [ ] Remove the fixed-height orb entirely. If a focal highlight is wanted, it
      must be a glow **anchored to and bounded by the flaring cell/column
      itself**, sized to the cell, and it fades out with that specific flare's
      own lifetime — never a separate persistent shape drifting on its own
      axis.
- [ ] Speaking continues the exact same flare mechanism as Thinking's decode
      tail (one continuous visual language across Thinking → Synthesising →
      Speaking, per your original ask), with audio acting only as a small
      capped modulation on top of real flares — never replacing or
      independently animating alongside them.

### Cross-cutting
- [ ] Audit every additive term in the decode/prefill formulas and remove any
      that do not map to a named, real signal (jitter terms kept only if they
      demonstrably serve legibility, not decoration for its own sake).
- [ ] MoE Constellation (already validated separately) is unaffected by this
      plan — only the dense/shared lattice path for Thinking/Speaking changes.

## Implementation Plan

- [ ] Task 1. Remove the fixed-position glow bloom (`cortex.rs:1721-1725`) and
      replace all decode-phase glow calls with per-flare, per-cell-anchored
      glow only.
- [ ] Task 2. Redesign prefill: sequential per-column latch driven by real
      prefill progress, with real per-row stagger/texture, replacing the
      single raised-cosine bump and the per-row-uniform column fill.
- [ ] Task 3. Replace the prefill→decode cross-fade with a one-shot
      deterministic transition triggered once on first real keyframe arrival.
- [ ] Task 4. Rework the decode data path so keyframes are consumed against a
      real timing clock (reusing the existing replay/TTS-sync clock) instead
      of `tick`-rate `live_cursor` advancement; verify no frame is
      skipped/aliased at real token rates above and below 30/s.
- [ ] Task 5. Rewrite the decode flare rendering to use true per-row values
      from the token's captured per-layer sample, replacing the flat
      column-fill approach.
- [ ] Task 6. Extend the same flare mechanism to Speaking, keeping today's
      capped audio modulation but re-verifying it never dominates or
      desynchronizes from the flare signal.
- [ ] Task 7. Self-review: render the offline gallery at the real panel size
      for Thinking (early/mid/late) and Speaking, inspect frames directly
      before touching the daemon, and confirm: no orb, visible per-row
      texture in every phase, no visible flash/pop at the prefill→decode
      boundary, and one clearly nameable motion per phase.
- [ ] Task 8. Run the full pre-commit gate (`cargo fmt`, `cargo clippy -D
      warnings`, `cargo test --workspace`), rebuild the daemon, and do a live
      check before committing.

## Verification Criteria

- No visual element exists that isn't anchored to and bounded by an actual
  grid cell or region (no floating/fixed-position shapes).
- Every phase's grid shows real row-to-row variation, not uniform column bars.
- The prefill→decode transition is a single visible event, not a
  multi-second blend or a visible flash/pop.
- Decode motion is verified to be paced by real token/playback timing at both
  a fast token rate and a slow one, with no dropped or doubled visual events.
- A person unfamiliar with the internals can watch the gallery frames and
  correctly describe, in one sentence, what each phase (prefill vs decode vs
  speaking) is showing, without being told in advance.

## Potential Risks and Mitigations

1. **Reworking the timing source (Task 4) touches the same replay-clock
   machinery already relied on for TTS sync in Speaking.**
   Mitigation: reuse the existing mechanism rather than building a second one,
   and explicitly test that Speaking's existing TTS-sync behaviour is
   unaffected before extending it into Thinking's decode tail.
2. **Per-row real texture requires a data shape (sub-layer samples) that may
   not exist at the same granularity for every model/backend.**
   Mitigation: where fine-grained per-row data isn't available (e.g. external
   backends without capture), fall back to a stable per-row deterministic
   variation function rather than a uniform fill, so texture is never lost
   even in the degraded case — but never present it as if it were real
   internals when it isn't.
3. **A one-shot deterministic transition could itself look abrupt if not
   tuned.**
   Mitigation: gallery-review the transition specifically at multiple points
   (early prefill, late prefill, immediately after first token) before
   accepting it, per Task 7's explicit acceptance bar.

## Alternative Approaches

1. **Minimal patch approach (rejected):** keep the current formula and just
   tune constants (fade speeds, glow radius, threshold). Rejected because the
   session's history already shows this pattern repeatedly failing — the
   defects are structural (competing blended signals, tick-rate-coupled
   timing, no row texture), not a matter of wrong numbers.
2. **Full re-adoption of an earlier explored concept (e.g. Neural Current
   flow-field or Deep Scan telemetry) for Thinking/Speaking specifically:**
   viable if this redesign still doesn't land well, but it would break visual
   continuity with the already-approved Listening spectrum and MoE
   Constellation. Held in reserve, not recommended as the first move.
