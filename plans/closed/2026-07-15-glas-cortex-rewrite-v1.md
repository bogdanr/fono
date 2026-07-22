# Glas Cortex Rewrite — "Watch it think" LED panel

## Status: Completed (visual rewrite shipped; Task 15 speech-synced clock deferred as independently-shippable follow-up)

## Objective

Replace the current Glas Cortex visualization (`crates/fono-overlay/src/cortex.rs`, ~3 000 lines, shipped as a "rough preview" in v0.16.0) with a from-scratch port of the new design specified in `/root/Downloads/IMPLEMENTATION.md` and prototyped in `/root/Downloads/fono-how-it-thinks.bundle.html`. The new design is a **fixed 6×46 LED bar** with an event-driven visual language: cool prefill flood, warm per-token compute sweeps, dense equalizer vs MoE expert lanes, confidence/entropy modulation, speech-paced timing, and never-dead resting behavior. The rewrite keeps the existing capture layer (`brain_tap`) and overlay wiring largely intact, extending the data contract only where the spec requires it.

## Context and key findings

- **Reference implementation is locked inside the bundle.** The algorithm lives in `cortex-live-engine.js`, gzip+base64-embedded in the bundle's `__bundler/manifest` script tag. It must be extracted before porting (Task 1). The spec (`IMPLEMENTATION.md`) is a full contract and can drive the port even if extraction is skipped, but the extracted JS is the authoritative source for constants and the `_cycleGrounded` timing path.
- **Data contract is ~90% already met** by `BrainEvent` (crates/fono-core/src/brain_tap.rs:134-150): `reply_begin{n_layer}`, `prefill{n_tokens}`, `frame{token_index, layer_norms, experts, token_prob, entropy_bits}`, `reply_end{total_tokens, gen_ms, …}`. Gaps: (a) `kind: "dense"|"moe"` must move into `reply_begin` (currently inferred downstream), (b) optional `n_experts_total`/`n_experts_active` for adaptive sparsity, (c) TTS word-boundary timestamps for the preferred speech clock.
- **Wiring can be preserved.** `RendererState` talks to the cortex only via `on_state`, `apply(CortexCmd)`, `tick`, `needs_animation_frames`, `clear`, and `draw_cortex` (crates/fono-overlay/src/renderer.rs:1671-1786, 2220-2235). Keeping this surface means zero changes to backends, the animation pump, or the tray.
- **Panel geometry differs from the mockup's native size.** Spec native panel is 507×67 (46 cols × 10 px cells, 1 px gap); fono's strip is 640×100 logical / ~810×96 physical. The grid stays fixed at 6×46; cell size is computed from panel bounds (square cells, integer-pixel gap, centered), exactly the discipline `Lattice::compute` already uses — but with **columns fixed at 46** instead of derived from width, and layers mapped `layer(col) = round(col/(COLS-1)·(nLayer-1))`.
- **Spec does not cover Listening/mic.** The spec covers idle/prefill/decode/speaking only. Assumption (documented): retain a mic-driven Listening scene, restyled onto the 6×46 grid using the cool ramp (column energy bars center-out, same visual grammar as the dense equalizer) so the whole style feels like one device.
- **Speech-synced clock (spec §6 priority 1)** needs word boundaries fono's TTS path doesn't emit today. Assumption: ship the grounded-replay clock first (spec §6 priority 2, clamped 0.30–1.70 s per real `token_index` gap, never looped), approximating speech pacing during `Speaking` with the existing `AudioTotal`/`AudioBands`/`PlaybackDone` commands (crates/fono/src/assistant.rs:1053-1090) to gate playback progress. True word-boundary emission is a follow-up task, kept in this plan as optional.

## Implementation Plan

### Stage 0 — Recover the reference algorithm

- [x] Task 1. Extract `cortex-live-engine.js` (and the demo `traces/*.json` if embedded) from `/root/Downloads/fono-how-it-thinks.bundle.html`: decode each `__bundler/manifest` entry (base64 → gunzip) with a throwaway script, save the engine JS to a scratch location (not committed), and identify the `Panel` render loop, `Field` constants, ramp stops, and `_cycleGrounded` timing code. Rationale: the spec references exact constants ("port this") that only exist in the JS.

### Stage 1 — Engine-side data contract (fono-core)

- [x] Task 2. Extend `BrainEvent::ReplyBegin` with `kind` (dense/moe enum) and optional `n_experts_total: Option<u32>` / `n_experts_active: Option<u32>` (crates/fono-core/src/brain_tap.rs:134-150). Populate `kind` and expert counts from llama.cpp model metadata at the point where `publish_reply_begin` is called (n_expert / n_expert_used hyperparameters are available on the loaded model). Rationale: spec §3.1 and §9.1 — row semantics and adaptive sparsity both key off `reply_begin`.
- [x] Task 3. Update `crates/fono-core/examples/brain_trace_dump.rs` to emit `kind` inside the `reply_begin` event (matching spec §3.1) while keeping the existing top-level summary fields, and fix its dangling plan reference on line 4 to point at this plan. Rationale: the trace JSON is the shared fixture format between the demo and the Rust renderer tests.

### Stage 2 — Wiring updates (fono-overlay lib + session)

- [x] Task 4. Extend the mirror types `CortexCmd`/`CortexFrame`/`CortexExperts` (crates/fono-overlay/src/lib.rs:160-223) with the new `ReplyBegin` fields, and update the pure mapping `cortex_cmd_from_brain_event` (crates/fono/src/session.rs:710-738) accordingly. Keep `OverlayCmd::Cortex`, `push_cortex`, and the slim-build stub untouched. Rationale: minimal-diff wiring; fono-core still cannot depend on fono-overlay.

### Stage 3 — The new renderer (rewrite cortex.rs)

- [x] Task 5. Replace the internals of `crates/fono-overlay/src/cortex.rs` with the ported engine while preserving the public surface consumed by `renderer.rs` (`CortexState::{on_state, apply, tick/tick_dt, clear, needs_animation_frames, set_model_layers}` and `draw_cortex(...)`). Delete the retired machinery: chart-recorder decode strip, prefill sweep latching, constellation, beads/sparks, MoE HUD, entropy skyline, `GlowAccum` bloom (spec §4.1 explicitly forbids blur — the head is rendered as stepped cells `1.0/0.66/0.42`). Rationale: clean-slate port per the user's directive; the API freeze keeps the blast radius inside one file.
- [x] Task 6. Implement the fixed grid: `COLS = 46`, `ROWS = 6`, square cells sized to the panel bounds with 1 px-equivalent gap (scaled, integer-aligned, centered), and the `layer(col)` mapping from spec §2. The core data structure is a single `field[6][46]` brightness/color-tier array, drawn per cell — no per-pixel effects. Rationale: spec mandates the grid never resizes to layer count, and the LED-cell aesthetic is the whole design.
- [x] Task 7. Implement the two fixed color ramps from spec §8 — cool `#0c0c28→#222a96→#2882d6→#3ce0d6→#e4fcff`, warm `#1a0c22→#782860→#d9342f→#ff8b5e→#fff7ec` — on near-black backing, replacing the accent-derived `accent_ramp`. Premultiplied-ARGB output as today. Rationale: spec §8; the accent coupling was part of the old design.
- [x] Task 8. Implement the phase behaviors: **Idle** slow amber sine breath drifting across columns; **Prefill** cool flood across all columns, amplitude `0.7 + 0.3·clamp(log(n_tokens+1)/log(400))`, one fast pass; **Decode dense** warm compute front per frame with center-out equalizer columns from log-normalized `layer_norms`; **Decode MoE** expert lanes (`lane = id % 6`, brightness from routing weights, faint ghost on unused lanes of active columns, adaptive lane count from expert ratio with co-activity gating per spec §9.1, lane-collision bumping); **Confidence/entropy** pulse brightness `0.5 + 0.5·token_prob`, width `lerp(1.1, 2.6, entropy)`, desaturation above normalized entropy 0.55. Rationale: spec §4–§5 verbatim.
- [x] Task 9. Implement never-dead behavior (spec §7): field decay `exp(-dt/0.30)` per tick, resting field at ~0.17 breathing render of last-known norms/routing between sweeps and during sampling gaps, idle breath when no reply is active. Rationale: strided capture (LAYER_STRIDE=4, governor-widened intervals) produces long frame gaps; the bar must stay alive.
- [x] Task 10. Implement the grounded-replay clock (spec §6.2): queue incoming frames, play once in `token_index` order, spacing by real token gaps scaled to a comfortable rate clamped 0.30–1.70 s, never looping. During `Speaking`, stretch/gate replay against audio playback progress (existing `AudioTotal`/`PlaybackDone` commands) so the trace roughly spans the utterance. Map `OverlayState` → phases as today (crates/fono-overlay/src/cortex.rs:460-477 semantics: Thinking covers prefill+early decode, Speaking covers playback). Rationale: real decode at 20–100 tok/s is unreadable; this is the spec's mandated demo-grade clock, upgraded later by Task 15.
- [x] Task 11. Restyle the Listening scene onto the new grid (documented assumption): mic spectrum binned to 46 columns, center-out row fill on the cool ramp, reusing the existing `push_fft_bins` tick path (crates/fono-overlay/src/renderer.rs:1753-1761). Rationale: fono needs a Listening state the spec doesn't define; visual grammar consistency beats inventing a second language.

### Stage 4 — Tests, examples, hygiene

- [x] Task 12. Rewrite `crates/fono-overlay/examples/cortex_gallery.rs` scenes for the new design (idle, prefill flood, dense decode, MoE lanes, high/low entropy, listening, dark + bright desktop composites, fractional HiDPI), keeping the offline-PPM regression harness shape. Keep `cortex_frame_bench.rs` (budget should comfortably improve — no bloom). Delete `crates/fono-overlay/examples/brain_mockups.rs` (its own header says delete once a concept is chosen). Rationale: gallery is the design-review and regression tool; mockups are obsolete.
- [x] Task 13. Port/replace unit tests in `cortex.rs` for the new invariants: grid never exceeds 6×46, layer(col) mapping endpoints, never-black during active reply, frames played exactly once in order, clamped spacing bounds, MoE lane selection determinism, entropy desaturation threshold. Rationale: the old tests assert retired behavior (e.g. `thinking_never_black_at_decode_latch`).
- [x] Task 14. Run the full pre-commit gate (`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --tests --lib`) and `./tests/check.sh --size-budget`; validate live with a real dense GGUF and a real MoE GGUF via the assistant flow and via `brain_trace_dump` replays; update `docs/status.md` (retiring the RC5 open issue and the "rough preview" caveat if confirmed fixed). No new crate dependencies are anticipated (pure software raster, all existing). Rationale: project hard rules.

### Stage 5 — Optional follow-up (separate sign-off)

- [ ] Task 15. Speech-synced clock (spec §6.1): emit word/viseme boundary timestamps from the TTS path (Piper exposes phoneme/word alignment; cloud voices may need estimated boundaries from text + duration), add a `CortexCmd::WordBoundary` command, and drive one sweep per spoken word aggregating the tokens behind it. Rationale: the spec's preferred clock; deferred because it crosses into the TTS provider layer and is independently shippable.

## Verification Criteria

- Gallery renders all new scenes at ~810×96 and they visually match the bundle demo side-by-side (extracted engine as reference).
- A real dense-model reply shows: cool flood on prefill → warm per-token fronts with equalizer columns → resting field during gaps → idle breath after `reply_end`; never a fully black panel mid-reply.
- A real MoE reply lights ≤3 expert lanes per column per spec §9.1 rules, with most of the column dark.
- Replay never loops frames and inter-sweep spacing stays within 0.30–1.70 s.
- `cortex_frame_bench` stays within the existing ~4 ms/frame budget (expected to improve).
- Pre-commit gate and size-budget gate pass; binary size does not grow (code removal likely shrinks it).

## Potential Risks and Mitigations

1. **Bundle extraction fails or the embedded engine diverges from the spec.**
   Mitigation: the spec is a complete standalone contract; port from §2–§8 constants directly and reconcile against the live demo opened in a browser.
2. **46 columns don't divide the physical strip cleanly (fractional HiDPI).**
   Mitigation: reuse the existing integer-alignment/centering discipline from `Lattice::compute`; add a 1.25× HiDPI gallery scene as a permanent regression (mirroring the old 7e scene).
3. **Strided/governed capture leaves `layer_norms` mostly zero per frame, making equalizer columns flicker.**
   Mitigation: spec §3.3 says hold last-known state — merge strided frames into a persistent per-layer array exactly as the resting field requires; only decay, never zero, on unobserved layers.
4. **Grounded replay clamped to ≥0.30 s/frame can outlast short TTS audio or lag long replies.**
   Mitigation: during Speaking, scale spacing so the queue spans `AudioTotal`; on `PlaybackDone`, fast-drain remaining frames within the clamp floor, then settle to resting/idle.
5. **Deleting ~2 500 lines breaks a hidden consumer of the old API (HUD, spectrum accessors used by renderer paint paths).**
   Mitigation: grep all `cortex.` call sites in `renderer.rs` before deletion; keep thin no-op shims only where a call site is retained deliberately, otherwise remove the call site in the same change.

## Alternative Approaches

1. **Adaptive column count (old behavior) instead of fixed 46**: better pixel usage on wide panels, but violates the spec's core "hardware geometry is fixed" rule and breaks the layer(col) contract — rejected.
2. **Incremental refactor of the existing cortex.rs**: lower short-term risk, but the two designs share almost no visual machinery (bloom vs. crisp cells, chart-recorder vs. sweeps, accent ramps vs. fixed ramps); a rewrite behind the frozen API is cheaper and cleaner — chosen.
3. **Implement speech-synced clock in the first pass**: best end-state UX, but couples the rewrite to TTS-provider changes across crates; staged as Task 15 so the visual rewrite ships independently.
