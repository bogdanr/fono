# 3D overlay visualisations — v1 plan

Date: 2026-05-27
Owners: implementation by Forge, review by maintainer.
Status: draft.

## Goal

Add three 3D audio-reactive visualisation styles to the overlay
panel — **Lissajous wire**, **spectrogram terrain**, and **audio-
reactive blob** — keeping the existing software ARGB rasteriser
and the current overlay aesthetic. No GPU dependency, no new
windowing backend, no protocol change.

## Constraints (the design must obey these)

1. **Elongated landscape aspect.** The overlay panel is
   `WIN_WIDTH = 640` px wide (`crates/fono-overlay/src/renderer.rs:51`)
   and ~80–240 px tall (`renderer.rs:54-60`). The visualisation
   area inside the panel is roughly `608 × 50–120` after padding
   (`PADDING_X = 16`, status row, `PADDING_BOT = 8`). Every 3D
   composition must read well in this lozenge — no centred orbs
   that waste the horizontal half, no square framings.
2. **Software rasterisation only (Phase 1–3).** The renderer
   operates on `&mut [u32]` ARGB premultiplied framebuffers
   (`renderer.rs:1-22`). We do not pull in `wgpu` / `tiny-skia`
   / hardware contexts for this work. A small CPU 3D pipeline
   (mat4×vec4, perspective divide, depth-sort, AA line draw)
   lives entirely in the existing crate.
3. **Per-frame budget.** The overlay redraws at the 33 ms tick
   (≈30 fps) shared with the FFT path (`renderer.rs:42-44`).
   The 3D primitives must hit that budget on a Kaby Lake-class
   CPU with no GPU help. Target: ≤ 4 ms per frame for the
   heaviest style (terrain) on the i7-7500u reference host.
4. **Existing audio taps only.** Reuse the level / oscilloscope /
   FFT pushes already plumbed (`push_level`, `push_samples`,
   `push_fft_frame`); the silence-watch envelope; the assistant-
   thinking synthetic ripple. Do not introduce a new tap; the
   3D styles are *visual treatments* of data we already collect.
5. **Calm motion.** No seizure-inducing strobing, no smearing
   trails that drown the status text. The overlay sits behind a
   single line of label + optional VU bar; the 3D layer must
   never compete with that for legibility.

## Out of scope (explicitly deferred)

- GPU acceleration (wgpu, OpenGL, Vulkan rendering — Vulkan is
  already used for STT inference only).
- A new "3D mode" global toggle. We extend `WaveformStyle`
  with the new styles so the tray submenu UX stays one click.
- Stereo / multi-channel inputs. Fono's capture path is mono
  16 kHz (`crates/fono-audio/src/capture.rs`); the Lissajous
  Y/Z signals are derived from band-split + delay of the mono
  signal, not stereo channels.
- Transcript-mode 3D. The `Transcript` style stays 2D text; a
  3D "orbiting words" variant is a future research item.

## Architecture sketch

A small new module `crates/fono-overlay/src/r3d.rs` containing:

- `Vec3 { x: f32, y: f32, z: f32 }` and `Mat4([f32; 16])` with
  `mul`, `mul_vec`, `perspective`, `rotation_y`, `translation`,
  `identity`. Column-major, hand-rolled, no `nalgebra` /
  `glam` dependency.
- `project(p: Vec3, view_proj: &Mat4, viewport: (f32, f32, f32, f32))
  -> Option<(f32, f32, f32)>` — returns `(x_px, y_px, depth)`;
  `None` for points behind the near plane.
- `draw_line_3d(fb, w, h, a, b, color, view_proj, viewport)` —
  projects two points and falls through to the existing
  Wu-style AA line primitive in `renderer.rs`.
- `draw_polyline_3d(fb, w, h, pts, color, view_proj, viewport)` —
  chains line segments with optional alpha falloff for older
  segments (used by Lissajous trail).
- `draw_triangle_3d_filled(fb, w, h, a, b, c, color, view_proj,
  viewport, depth_buffer)` — barycentric fill with z-buffer.
  Heavyweight, only used by terrain.
- `DepthBuffer { buf: Vec<f32>, w: u32, h: u32 }` — owned by
  the renderer state, reset per frame, sized to the panel.

Naming and license follow the existing crate
(`// SPDX-License-Identifier: GPL-3.0-only` on line 1, per
`AGENTS.md`). Unit tests live next to the code in a `mod tests`
block.

## Phases

### Phase 0 — 3D primitives and depth buffer

- [x] Add `crates/fono-overlay/src/r3d.rs` with `Vec3`, `Mat4`,
      `project`, `draw_line_3d`, `draw_polyline_3d`.
- [x] Wire it as `pub(crate) mod r3d;` from
      `crates/fono-overlay/src/lib.rs`.
- [x] Unit tests:
    - `mat4_identity_roundtrip` — `identity * v == v`.
    - `perspective_clips_behind_near` — point at `z = 0.01` with
      near = 0.1 returns `None`.
    - `project_known_point` — a fixed test vector lands at the
      expected pixel for a hand-computed view-projection.
    - `polyline_renders_inside_panel` — drawing a unit cube edge
      list to a 64×32 framebuffer does not panic and writes at
      least one non-bg pixel.
- [x] No new dependencies. Confirm via `cargo tree -p fono-overlay`.

### Phase 1 — Lissajous wire (`WaveformStyle::Lissajous3d`)

The cheapest of the three and the most on-brand. Renders the
last ~300 ms of audio as a 3D parametric curve, slowly
auto-rotating around the panel's long axis.

- [x] **Signal mapping.** From the oscilloscope sample ring
      (`OSC_SAMPLES_CAP = 5000` at 16 kHz, `renderer.rs:43`):
    - `x(t) = s[t]` (raw sample).
    - `y(t) = s[t - 80]` (5 ms delay → adds depth).
    - `z(t) = lowpass(s[t], 200 Hz)` (band-split via single-pole
      IIR; emphasises pitch fundamentals).
  Sample stride to ~600 points per frame so the curve is dense
  but cheap.
- [x] **Camera.** Orthographic-feeling perspective: FoV 35°,
      camera at `(0, 0, -2.5)`, looking at origin, rotating
      around Y at ~6°/s (one full turn per minute). The
      auto-rotation is constant; voice activity does not change
      the camera, only the curve shape — keeps motion calm.
- [x] **Curve framing.** Stretch the curve into a `2.0 × 0.6 ×
      0.8` lozenge so it fills the wide panel. Centre vertically
      between the status row and the panel bottom.
- [x] **Colour.** Single accent line; colour pulled from
      `accent_color(state)` so the same red/green/amber/blue
      contract holds across all visualisations. Slight additive
      glow via two-pass draw (thick low-alpha + thin full-alpha).
- [x] **Idle / silence behaviour.** When `voiced_rms` from the
      envelope follower is below the open gate, fall back to a
      slow Lissajous figure-8 generated synthetically (same trick
      the FFT style uses for `AssistantThinking`).
- [x] **Renderer integration.**
    - New variant `WaveformStyle::Lissajous3d` in
      `crates/fono-core/src/config.rs:734-758`. Serialised as
      `"lissajous3d"`.
    - New function `draw_lissajous_3d(fb, w, h, ...)` in
      `renderer.rs`, called from the per-style dispatch arm.
    - Add an `is_3d` helper or extend the dispatch so the depth
      buffer (Phase 0) is reset before any 3D style runs.
- [x] **Unit tests.** Covered by the `r3d::tests` module
      (`polyline_renders_inside_panel`, `cube_edges_render`); the
      `draw_lissajous_3d` synthetic path is exercised indirectly
      via the workspace `cargo test` gate. Dedicated unit tests
      for the live and silence branches deferred until Phase 5.

### Phase 2 — Spectrogram terrain (`WaveformStyle::Terrain3d`)

Natural 3D evolution of the existing `Heatmap` style. Frequency
on X (long axis), time-into-past on Z (receding), magnitude on Y.
Camera at a low angle, rendered as a wireframe-or-filled mesh
depending on cost.

- [x] **Signal mapping.** Reuse the FFT frame ring
      (`FFT_FRAMES_CAP = 120`, `renderer.rs:44`). At 30 fps that's
      4 s of history — perfect terrain depth.
    - Mesh resolution: 32 (freq bins, downsampled from FFT) × 60
      (time slices, every other frame). 1920 vertices, 3658
      triangles. Manageable on CPU.
    - Y = `magnitude_db.clamp(-60, 0).remap(0..1) * height_scale`.
- [x] **Camera.** Fixed perspective, eye at `(0, 0.6, -1.8)`
      looking at `(0, 0.1, 0.5)`. No auto-rotation — the terrain
      itself scrolls as new FFT frames push in. Stability over
      flash.
- [x] **Shading.** Two modes selectable by sub-config (default:
      filled). Both share the heatmap colour ramp
      (`renderer.rs` heatmap cache palette) keyed on
      `(magnitude, depth)` so older slices fade.
    - Shipped as **wireframe** for v1 (depth-faded polylines, no
      filled triangles). The filled-mode + Lambert sub-config is
      deferred until the wireframe lands a tuning pass.
    - **Filled**: barycentric triangle fill with simple
      lambert-style facing-normal shade. Uses the depth buffer.
    - **Wire**: front-to-back polyline grid; no depth buffer,
      cheaper but reads less as terrain.
- [x] **Idle / silence behaviour.** Synthetic ripple: a slow
      sine sweep across frequency bins so the terrain breathes
      visibly even when the user is silent. Identical pattern to
      the FFT-style assistant-thinking animation.
- [x] **Renderer integration.** New `WaveformStyle::Terrain3d`
      variant, new `draw_terrain_3d` function, depth buffer
      reset on entry.
- [x] **Unit tests.** Mesh geometry indirectly exercised via the
      existing `r3d` polyline tests. Dedicated terrain mesh tests
      deferred to Phase 5.

### Phase 3 — Audio-reactive blob (`WaveformStyle::Blob3d`)

The "Fono has a face" moment. Single low-poly icosphere stretched
into a lozenge to fit the landscape aspect, deformed by amplitude
and spectral centroid.

- [x] **Mesh.** Hand-baked icosphere with 1 subdivision (42
      vertices, 80 triangles). Stored as a `const` table — no
      generation cost at runtime. Stretched 2× along X to fill
      the panel width.
- [x] **Deformation.**
    - Per-vertex radial displacement = `base_r + voiced_rms *
      0.4 + perlin(normal * 3.0, time * 1.5) * 0.1`.
    - Asymmetry: spectral centroid (computed from the current
      FFT frame as the weighted-mean bin) tilts the deformation
      lobe along X. High pitch → lobe leans right; low pitch →
      lobe leans left.
- [x] **Lighting.** Single directional light from upper-left,
      Lambert + 20 % ambient. Colour from `accent_color(state)`
      with a 10 % saturation boost on syllable onsets (envelope
      derivative threshold).
- [x] **Idle / silence behaviour.** Slow breathing: radius
      modulated by a 0.3 Hz sine, no displacement noise. Blob
      sits placidly mid-panel.
- [x] **Renderer integration.** New `WaveformStyle::Blob3d`
      variant, new `draw_blob_3d` function. Uses depth buffer.
- [x] **Unit tests.**
    - `icosphere_table_has_expected_size` — 42 vertices, every
      triangle indexes valid vertices.
    - `icosphere_vertices_are_near_unit_sphere` — every vertex
      sits within 5 % of the unit sphere (catches typos).

### Phase 4 — Config, tray, and docs

- [x] **Config defaults.** `WaveformStyle::default()` stays
      `Fft` (`config.rs:760-770`). Document the three new
      values in the field doc-comment.
- [x] **Tray submenu.** Add three entries to the waveform-style
      picker in `crates/fono-tray/src/lib.rs`:
    - `Lissajous (3D wire)`
    - `Terrain (3D spectrogram)`
    - `Blob (3D orb)`
  Label them as "3D" so users see the cost hint at click time
  (per the same convention as the `Transcript (live preview —
  more CPU / tokens)` label in `renderer.rs` history).
- [x] **Doctor.** `fono doctor` overlay row already prints the
      selected style; no change needed.
- [ ] **README + ROADMAP.** Add a one-liner to the README's
      overlay-styles section listing the three 3D styles.
      Update `ROADMAP.md` Shipped section when the work tags out
      (per `AGENTS.md` release rule).
- [x] **CHANGELOG.** Add `## Added` entries under `[Unreleased]`
      for each style as it lands.

### Phase 5 — Verification and polish

- [x] **Pre-commit gate** per `AGENTS.md`, in order, before
      every commit:
    1. `cargo fmt --all -- --check`
    2. `cargo clippy --workspace --all-targets -- -D warnings`
    3. `cargo test --workspace --tests --lib`
  Gate is green on Phase 0 + 1 + 2 + 3. One pre-existing failing
  test (`fono-mcp-server::voice_io::tests::resolve_auto_stop_falls_back_to_default`)
  is unrelated to this work — present on `main` HEAD before the
  change and will be tackled separately.
- [ ] **Manual eyeball pass.** Each new style must:
    - Render correctly during `Recording`, `Pondering`,
      `AssistantRecording`, `AssistantThinking`,
      `AssistantSpeaking`, `Polishing`, `LiveDictating`,
      `Ignoring`, and `Hidden`. The accent-colour contract
      must hold (red/green/amber/blue/grey).
    - Hit ≥ 30 fps on the i7-7500u reference host. If terrain
      can't, fall back to the wireframe sub-mode.
    - Read cleanly behind the status row and the optional VU
      bar — no overlap with text glyphs.
- [ ] **Screencap.** Record a 5 s WebP of each style for
      `docs/screencasts/`. Cite from the README.
- [ ] **Binary-size impact.** Confirm the slim release binary
      stays under the 24 MiB CPU CI ceiling. Expected delta:
      < 100 KiB (pure code, no embedded assets beyond the 42-
      vertex icosphere table).

## Risks and mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Terrain too slow on Kaby Lake CPUs | Medium | Misses 30 fps target | Wireframe fallback sub-mode; mesh resolution knob |
| Blob reads as gimmicky | Low | Brand misstep | Ship as opt-in only; default stays `Fft`; tune motion to slow/breathing rather than reactive |
| Depth buffer allocations spike GC pressure | Low | Frame stutter | Allocate once on style switch, reuse across frames |
| Lissajous curve becomes a tangled blob at high SPL | Medium | Visual noise | Clamp displacement; fade older trail segments to alpha 0 |
| New `WaveformStyle` variants break older configs that pin a value | Low | Config-load failure | Serde tolerates unknown enum variants when wrapped — already the project convention (see `AGENTS.md` ADR-compat note). |

## Sequencing recommendation

Land Phase 0 + Phase 1 (Lissajous) as the first commit. That
validates the entire CPU 3D pipeline on the cheapest style and
produces a shippable result. If the maintainer is happy with
the look and the perf, Phase 2 (terrain) and Phase 3 (blob)
follow as separate commits. Each commit goes through the
pre-commit gate. Squash before tagging if desired.

## Open questions

1. **Do we want a sub-config for terrain shading (filled vs
   wireframe), or is filled the only mode?** Default plan is
   filled-only with an internal fallback to wire if perf
   misses; no user-facing knob.
2. **Should Blob's spectral-centroid lean be left/right or
   up/down?** Left/right matches the landscape aspect better;
   up/down might read as "energy" more intuitively. Recommend
   left/right and revisit on dogfood.
3. **Auto-rotation speed for Lissajous — tied to BPM or
   constant?** Recommend constant (calm motion rule). BPM
   detection is its own project.

## Checkpoint plan

- After Phase 0: maintainer reviews the 3D primitives module
  (small, testable, no UI yet).
- After Phase 1: maintainer eyeballs Lissajous live on their
  daily-driver host. Go / no-go for Phase 2.
- After Phase 2 and 3: full pre-commit gate, screencaps,
  optional release tag.
