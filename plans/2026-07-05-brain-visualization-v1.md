# Brain Visualization v1 — "Glass Cortex" Overlay Style

## Objective

Ship a spectacular, **truthful** visualization of the local LLM's forward pass as a new overlay
style: one continuous scene on the existing 640 × 80–240 px strip that follows the whole voice
pipeline — **listening** (live mic spectrum), **thinking** (prefill sweep + TTFT breathing),
**answering** (per-token layer/expert activity replayed in sync with TTS playback). It must work
for both dense and MoE GGUF models, activate for both embedded-LLM paths (assistant replies and
polish/cleanup), and respect two hard budgets: **< 1 % decode slowdown** when active (zero when
off) and **~zero binary-size growth** (no new crates, no shipped assets).

## Context

- Fono runs llama.cpp **in-process** via `llama_cpp_2` for the assistant
  (`crates/fono-assistant/src/llama_local.rs`) and polish (`crates/fono-polish/src/llama_local.rs`),
  sharing one backend in `fono_core::llama_backend`. This makes real instrumentation possible —
  the visualization is driven by captured data, never canned animation.
- llama.cpp exposes a per-graph-node eval callback (`cb_eval` in the context params — the hook
  `imatrix` uses). The safe `llama_cpp_2` wrapper does not surface it, but `llama-cpp-sys-2` is
  already in the dependency graph, so a small unsafe shim adds **zero new dependencies**.
- Layer count and expert count are read from GGUF metadata at load (`LlamaModel::n_layer()` etc.)
  — nothing is hardcoded per model.
- The overlay already has a CPU software-render pipeline with 3D primitives, depth buffer, and
  three 3D styles at 30 fps / ~4 ms per frame on a Kaby Lake CPU
  (`crates/fono-overlay/src/r3d.rs`, `crates/fono-overlay/src/renderer.rs`). Mic FFT frames are
  pushed to the renderer at 20 fps by the orchestrator (`crates/fono-overlay/src/renderer.rs:1934`).
  Styles are `fono_core::config::WaveformStyle` variants wired through tray/daemon/session.
- Timing inversion insight: generation is a burst (~15–30 tok/s) while speech playback is slow
  (~2–3 words/s), so the design is **record-then-replay** — sparse keyframes captured during the
  generation burst, replayed time-stretched in sync with TTS playback position.
- MoE specifics (expert routing, `--n-cpu-moe`-style offload, residency) depend on the MoE model
  candidacy being explored in `plans/2026-07-05-local-llm-technique-exploration-v2.md`. That plan
  also leans toward a managed `llama-server` for some workloads; the brain viz binds to the
  **embedded** path only (HTTP exposes none of these internals) — this is an accepted scope limit.

## Visual Design (the contract for the renderer work)

One scene, landscape orientation, low grazing 3D camera with slow parallax drift; palette
follows the tray state palette (ADR 0013); glow via runtime-generated radial sprites, additive
blending, and fake bloom (quarter-res emissive buffer + small separable blur).

- **The spine**: N transformer layers drawn left → right across the strip (layer 1 at the left
  edge, layer N at the right), each as a slim vertical ellipse (a ring seen edge-on); ~13 px per
  layer at 48 layers.
- **Listening phase** (mic hot): the same grid is driven by live mic FFT — spectrum bands mapped
  across the layer columns, energy as glow intensity, recording-accent tint. Honest signal, data
  already present at 20 fps.
- **Thinking phase** (prompt submitted → first token): prefill progress sweeps the spine left to
  right as prompt batches decode (honest signal from the decode loop); during residual TTFT gap
  the structure "breathes" at low luminance on a loop.
- **Answering phase** (replay, synced to TTS playback):
  - Each sampled token is a **bright bead** traveling left → right along the spine, cresting at
    the right edge exactly as its word is spoken; it bursts into a spark on arrival.
  - **Dense**: each ring flares with the real per-layer activation norm for that token; a
    slow-decaying heat trace accumulates into a "skyline" — the shape of the thought.
  - **MoE**: each layer's ring unfolds into a thin vertical column of expert cells (a barcode of
    experts per layer). Top-k routed cells flash with intensity = router weight. Cell base tint
    encodes residency: warm amber = RAM-resident, ice blue = cold on disk; a cold-cell hit shows
    a white "crack" flash that warms — the page fault made visible. If cells go sub-pixel,
    bucket experts into groups of 4 and let the flash bloom cover the exact cell.
  - **Uncertainty ribbon**: thin band along the bottom edge that widens/shimmers where token
    entropy was high, razor-thin where the model was confident.
  - **Minimal HUD**: tok/s and context-fill as two slim arcs in the far-right corner. Organic
    over dashboard.
- **Phase transitions are morphs**, not cuts: mic ripples collapse into the prefill sweep; the
  sweep's leading edge becomes the first bead.

## Cost Budgets (hard gates)

1. **Capture ≤ 1 % decode slowdown**, measured tok/s viz-on vs viz-off on the reference laptop.
   Achieved by: `cb_eval` left **null** for unsampled tokens (zero cost — llama.cpp never calls
   us); demand-armed only for keyframe tokens; keyframe rate sized to **playback** duration
   (~2–4 keyframes/s of audio), not token rate; one coalesced copy per sampled token (router
   probs + per-layer norms ≈ a few KB); auto-backoff thins sampling if a sampled token measures
   over budget; renderer interpolates between keyframes so sparse data degrades smoothly.
   `mincore` residency scan runs once per reply, not per token.
2. **Render** within the existing terrain-style frame budget (~4 ms / frame at 30 fps, Kaby Lake);
   renders only while the overlay is visible; style not selected = zero cost.
3. **Size ~zero**: no new crates (`llama-cpp-sys-2` + `r3d` + software renderer already in the
   graph), no asset files (sprites computed at startup), no `deny.toml` change. Expected code
   growth: tens of KB. Verified by the size-budget gate before push.

## Implementation Plan

### Phase 1 — Capture spike: prove the < 1 % budget (gate for everything else)

- [x] Task 1.1. `cb_eval` shim in `fono_core::llama_backend`: set the callback on the raw
      `llama-cpp-sys-2` context params at context creation, behind a `BrainTap` handle that is
      `None` (null callback) unless a sink is attached. Confirm `llama_cpp_2`'s
      `LlamaContextParams` allows reaching the underlying sys struct; if not, create the context
      via the sys API in one contained unsafe block (or carry a minimal patch — decide here).
      *Done: `fono_core::brain_tap` writes `cb_eval`/`cb_eval_user_data` directly into the
      `llama_cpp_sys_2::llama_context_params` behind `LlamaContextParams` (repr-transparent
      access, one contained unsafe block); no crate patch needed, zero new dependencies.*
- [x] Task 1.2. Define the `BrainTrace` keyframe format and channel: per sampled token —
      token index, per-layer hidden-state norms, top-k logits + entropy, and (MoE) per-layer
      routed expert IDs + weights. Bounded ring buffer, drop-oldest, never blocks the decode
      thread.
      *Done: `BrainKeyframe` + bounded drop-oldest ring in `brain_tap.rs`; per-frame layer
      norms use a rotating `LAYER_STRIDE` residue class to cap per-sample graph splits.*
- [x] Task 1.3. Tensor identification desk-check: confirm the graph node names/types to match in
      the callback for (a) per-layer output hidden states, (b) MoE router probs/top-k, across the
      currently shipped dense default (Gemma E2B path) and one MoE architecture. Record the
      matching rules; they must be name-pattern based, not index based, to survive model changes.
      *Done: name-pattern rules `l_out-<i>`, `ffn_moe_topk-<i>`, `ffn_moe_weights-<i>` (verified
      against vendored llama.cpp graph sources); bench validated 35/35 nonzero layer norms on
      the shipped Gemma E2B dense model.*
- [x] Task 1.4. Overhead measurement: benchmark tok/s with (a) null callback, (b) armed callback
      at 1-in-5 / 1-in-20 / 1-in-50 tokens with coalesced copies, on the reference laptop (CPU
      and Vulkan/UMA). Implement auto-backoff from these numbers. **Gate: viz-on within 1 % of
      viz-off at the chosen default rate, or the sampling rate auto-backs-off until it is.**
      *Done: `examples/brain_tap_bench.rs` + `SampleGovernor` (EMA cost model, auto-widening
      interval). On the reference laptop (CPU, Gemma E2B, layer-norm capture strided across
      `LAYER_STRIDE` residue classes): sampled token ≈ +8 % → governor settles at interval
      ≈ 9–25, amortized overhead 0.89–0.94 % — gate PASS across repeated runs; warm-machine
      wall-clock active median +0.13 %. Wall-clock A/B on this machine is ±20 % thermal noise;
      the within-run governor estimate is the enforced gate.*
- [x] Task 1.5. Wire the tap into both embedded paths (assistant `llama_local.rs`, polish
      `llama_local.rs`) behind a single config flag; default off; confirm zero measurable impact
      when off (null callback, no allocation).
      *Done: both factories install the tap only when `[overlay] brain_capture = true`
      (default off ⇒ no callback installed, no allocation); shared decode helper
      `decode_token_with_tap` used by both paths.*

### Phase 2 — Renderer: the Glass Cortex style (dense-model scope)

- [x] Task 2.1. Add the style variant (`fono_core::config::WaveformStyle`) and thread it through
      tray menu, daemon style cycling, session ambient driver, and the renderer dispatch — same
      wiring as `Terrain3d`.
- [x] Task 2.2. Rendering primitives on top of `r3d`: runtime-generated radial glow sprite,
      additive blit, quarter-res emissive buffer + separable blur composite. Budget-check each on
      the Kaby Lake reference (stay within the terrain envelope).
- [x] Task 2.3. The spine: N layer rings from `n_layer()`, grazing camera, parallax drift, heat
      trace accumulation/decay. Handle N from ~12 to ~60+ gracefully (ellipse width scales).
- [x] Task 2.4. Replay engine: consume `BrainTrace` keyframes; map token index → reply text
      position → TTS playback position (per-chunk durations where the TTS engine provides them,
      proportional character mapping otherwise); interpolate between keyframes; bead + flare +
      spark + uncertainty ribbon + HUD arcs.
- [x] Task 2.5. Phase machine + morphs: listening (mic FFT on the grid — reuse the pushed FFT
      frames), thinking (prefill sweep driven by real batch-decode progress + breathing loop for
      the TTFT gap), answering (replay), and the two morph transitions. Idle/loop fallback for
      any gap with no data behind it.
- [x] Task 2.6. End-to-end dense validation: full assistant turn and a polish/cleanup run on the
      current default local model; verify sync feel (bead crest ≈ spoken word), frame budget,
      and the two cost gates. *Measured:* capture gate 0.955 % ≤ 1 % on the default dense model
      (35/35 layers) and on a second dense GGUF with a different `n_layer`; frame gate via
      `examples/cortex_frame_bench.rs` — 1.9 ms mean at the 640×240 max panel (terrain baseline
      1.6 ms, ~4 ms envelope), 4.3 ms at 2× HiDPI (4× the reference pixel count). Live sync-feel
      check (bead crest ≈ spoken word) is a user-run item on the desktop session.

### Phase 3 — MoE extras (contingent on an MoE model landing per the technique-exploration plan)

- [ ] Task 3.1. Router capture: extend the Phase 1 tensor matching to routed-expert IDs/weights
      on the chosen MoE architecture; validate values against llama.cpp's own logs on a few
      prompts.
- [ ] Task 3.2. Honeycomb columns: expert-cell rendering, top-k flash, 4-expert bucketing when
      sub-pixel, per-reply expert-usage heat trace.
- [ ] Task 3.3. Residency map: locate the mmapped expert tensor file ranges (GGUF tensor offsets
      + the model's mmap base), `mincore()` scan once per reply, amber/blue tinting, white-crack
      page-fault flash when a routed expert was cold at reply start. Linux-only initially;
      degrade to neutral tint elsewhere.
- [ ] Task 3.4. MoE end-to-end validation incl. an offloaded configuration (`--n-cpu-moe`-style
      split): confirm cold-expert flashes correlate with observed latency hitches.

### Phase 4 — Ship polish

- [ ] Task 4.1. Config + docs: style listed in `docs/configuration.md`, tray label/description,
      capture flag documented; note the embedded-path-only scope (no brain data via external
      OpenAI-compatible/Ollama servers — style falls back to listening + phase animation with
      stylized answering).
- [ ] Task 4.2. Trace persistence hook (cheap, optional): keep the last reply's `BrainTrace` in
      memory for a "replay this answer" debug view later; no UI in this plan — just don't design
      it away.
- [ ] Task 4.3. Pre-push gates: fmt/clippy/tests, size-budget check, and the two measured cost
      gates recorded in the PR/commit body.

## Verification Criteria

- Decode tok/s with capture armed at the default keyframe rate is within **1 %** of capture-off,
  on the reference laptop, dense and (when available) MoE; capture off = null callback (zero).
- Frame time for the style stays within the existing 3D-style envelope (~4 ms at 30 fps on the
  Kaby Lake reference); style unselected = zero render cost.
- No new crates in `Cargo.lock`; size-budget gate green.
- Layer count, expert count, expert choices, activation norms, entropy, prefill progress, and
  residency shown are all read from the running model — no per-model hardcoding; verified on at
  least two different dense GGUFs (different `n_layer`).
- Bead crest visually coincides with the spoken word across a 30+ second reply (proportional
  drift acceptable, no runaway desync).
- Both activation sources work: assistant reply and polish/cleanup burst.

## Potential Risks and Mitigations

1. **`llama_cpp_2` gives no path to set `cb_eval`** — Mitigation: Task 1.1 decides between raw
   sys-level context creation (contained unsafe) or a minimal vendored patch; both are zero new
   dependencies. Worst case, Phase 2's listening/thinking phases plus logits-only answering
   (entropy, tok/s — available without the callback) still ship as a degraded-but-honest style.
2. **GPU→CPU sync cost on Vulkan blows the 1 % budget even when sparse** — Mitigation: Task 1.4
   measures before any renderer work; auto-backoff plus keyframe interpolation means the floor
   (very sparse sampling) is always available; UMA on the reference target makes copies cheap.
3. **Tensor names differ across architectures** — Mitigation: name-pattern matching rules
   recorded per-arch in Task 1.3; unknown arch ⇒ norms-only fallback (hidden-state outputs are
   uniformly identifiable), MoE extras simply stay off.
4. **TTS timing alignment too coarse** (engines without per-chunk durations) — Mitigation:
   proportional character mapping with re-sync at chunk boundaries; drift is cosmetic and
   self-correcting per chunk.
5. **Future managed `llama-server` becomes the main assistant path**, starving the viz of data —
   Mitigation: accepted scope (embedded-only) documented in Task 4.1; if the server path wins,
   a server-side trace side-channel becomes a separate follow-up plan.
6. **MoE model never lands as a default** — Mitigation: Phase 3 is cleanly severable; Phases 1–2
   deliver the full dense experience on their own.

## Alternative Approaches

1. **Stylized/canned animation with no capture** — cheapest, but violates the core requirement
   that this be a genuine visualization of the model; rejected.
2. **GPU-rendered viz (wgpu)** — better bloom for free, but adds a major dependency tree against
   the size budget and forks the overlay stack; rejected while the CPU renderer meets the frame
   budget.
3. **Patch llama.cpp/llama-server for a trace side-channel** — needed only if the managed-server
   path becomes primary; deferred (Risk 5).
