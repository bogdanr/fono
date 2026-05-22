# Fono website FFT visualisation demo

## Status: Completed

## Objective

Ship a self-contained, dependency-free JavaScript + HTML5 Canvas demo
that lives inside a 600 × 300 px slot on the Fono marketing site and
faithfully simulates the Fono overlay's **FFT** waveform style. The
demo doubles as a product showcase ("this is what Fono looks like on
your desktop") and as a commercial loop that plays as soon as the page
loads — no microphone permission, no network calls, no audio
playback. It must look indistinguishable from a real recording session
captured with `[overlay].style = "fft"`.

Layout inside the 600 × 300 box:

- **Top half (≈ 600 × 150 px):** the live overlay panel — rounded
  charcoal background, left accent stripe, "RECORDING" / "LIVE"
  status label, and the 300-bar FFT spectrum. This is the same
  geometry the real overlay paints at 640 × 100, scaled and
  re-proportioned for the site canvas.
- **Bottom half (≈ 600 × 150 px):** a "dictated text" surface that
  reveals a scripted phrase character-by-character, phase-locked to
  the synthetic audio envelope so the words appear *as if* the user
  were speaking them. Acts as the marketing caption.

The demo is one `.html` snippet (or `<canvas>` + `<script>` pair) that
can be dropped into the existing site (Hugo / plain HTML / GitHub
Pages — site repo is separate from this repo, so the deliverable is
portable markup, not a build target inside `fono/`).

## Source-of-truth references (Fono repo)

Algorithms and constants to mirror exactly:

- `crates/fono/src/session.rs:49-80` — FFT window size 4096, max
  3 kHz, 300 display bins, dB floor −20, dB ceiling +30, Hann
  window, 50 ms tick.
- `crates/fono/src/session.rs:665-771` — producer side: Hann window,
  `realfft` magnitude, source-bin → display-bin slice averaging,
  dB conversion and `[0, 1]` normalisation, `push_fft_bins`.
- `crates/fono-overlay/src/real.rs:786-853` — `draw_fft`: bar geometry
  (pixel-aligned, no AA at seams), alpha curve
  `0x33 + (0xFF − 0x33) · v`, floor line at `COLOR_TEXT_DIM` α 0x33.
- `crates/fono-overlay/src/real.rs:259-287` — accent colours and
  state labels (`RECORDING` = 0xFFE05454, `LIVE` = 0xFF637AFF,
  etc.) and `state_label`.
- `crates/fono-overlay/src/real.rs:218-253` — panel geometry: width
  640 logical px, padding 24 / 14 / 16, accent width 4, corner
  radius 12, status font 13 px, status-to-content gap 14,
  `COLOR_BG = 0xCC17171B`, `COLOR_TEXT = 0xFFECECF1`,
  `COLOR_TEXT_DIM = 0xFFAAAAB2`.
- `crates/fono-overlay/src/real.rs:1611-1652` — redraw order: clear
  to transparent, rounded panel, accent stripe, status label,
  content area.

The JS port does **not** need to be byte-identical — the colour bytes
and bar alpha curve are what carry the visual signature. Match those
literally; everything else can be scaled to fit the 600 × 300 box.

## Implementation Plan

- [ ] Task 1. **Carve out the demo surface on the website.** Coordinate
  with the site repo (separate from `fono/`) to reserve a 600 × 300
  area in the hero / "see it in action" section, with a single
  `<canvas id="fono-demo" width="600" height="300">` element and a
  small `<noscript>` fallback that shows a still PNG of the same
  scene so the visual story survives JS-off / reader-mode viewers.
  Rationale: the site is the showcase venue; without a stable slot
  there is no place to land the script.

- [ ] Task 2. **Synthesise a deterministic "voice" signal in JS.**
  Implement a small additive-synthesis generator that produces 16 kHz
  mono `Float32` samples representing a plausible speaking voice:
  - A slowly drifting fundamental F0 between ~110 Hz (male-ish) and
    ~220 Hz (female-ish) per "utterance" so the spectrum shifts
    over the loop.
  - 5 – 8 harmonics with formant-shaped amplitudes (boost around
    ~500 Hz, ~1.5 kHz, ~2.5 kHz — the first three vowel formants)
    so the visual spectrum has real structure inside the
    0 – 3 kHz band the overlay shows.
  - A syllable envelope (Gaussian bursts every ~180 – 260 ms,
    plus brief silent gaps between phrases) so bars rise and fall
    in believable cadence rather than droning.
  - A pink-ish noise floor at −40 dB so quiet bins flicker the way
    breath / room tone does in the real capture.
  Use a seeded PRNG (e.g. mulberry32) so every visitor sees the
  same loop and screenshots are reproducible. The generator runs
  on a virtual clock advanced by `requestAnimationFrame` deltas; no
  `AudioContext` is created and nothing is ever played out — the
  demo is silent. Rationale: avoids autoplay restrictions and
  microphone prompts, gives full control over what the spectrum
  looks like, and matches Fono's privacy-first story (no audio
  leaves the user).

- [ ] Task 3. **Implement a minimal radix-2 real FFT in JS.** Port the
  producer math from `crates/fono/src/session.rs:665-771`:
  buffer the last 4096 samples; apply a Hann window
  (`0.5 − 0.5·cos(2π·i / (N − 1))`); run an N = 4096 FFT (custom
  radix-2 Cooley-Tukey on `Float32Array` real/imag buffers, ~12 KB
  of code, no dependencies); take magnitudes via `hypot(re, im)`;
  truncate to the source bins covering 0 – 3 kHz
  (`max_source_bin = round(3000 · 4096 / 16000) = 768`); bucket
  those 768 source bins into 300 display bins by averaging the
  `[start, end)` slice each display bin owns (same integer
  multiply-divide mapping the Rust code uses); convert each
  averaged magnitude to dB (`20 · log10(mag.max(1e-6))`); normalise
  with `(db − −20) / (30 − −20)` clamped to `[0, 1]`. Push one frame
  per 50 ms of virtual audio (20 fps), which means every animation
  frame at 60 fps either renders the previous bin set or computes a
  new one depending on virtual-clock progress. Rationale: this is
  what carries the "looks like Fono" signature — same window, same
  band, same dB curve, same bin count.

- [ ] Task 4. **Draw the upper-half overlay panel.** Replicate
  `crates/fono-overlay/src/real.rs:1611-1865` for the FFT path,
  scaled to the 600 × 150 top half:
  - Clear the whole 600 × 300 canvas to transparent (or to the page
    background) once per frame.
  - Fill a rounded rectangle for the panel using
    `rgba(23, 23, 27, 0.80)` (= `0xCC17171B`) with a 12 px corner
    radius. Use `ctx.roundRect` where available, fall back to a
    manual path.
  - Paint the left accent stripe (4 px wide, slightly inset top /
    bottom by 0.4 × radius) in the chosen accent colour. Default to
    indigo `#637AFF` ("LIVE") for the showcase; expose a
    `data-state` attribute on the canvas so the site can switch to
    red `#E05454` ("RECORDING") for an alternate hero variant
    without code changes.
  - Render the status label ("LIVE" or "RECORDING") at 13 px in
    `#AAAAB2` at the same `PADDING_X + ACCENT_WIDTH` inset and
    `PADDING_TOP + STATUS_FONT_PX × 0.85` baseline the Rust code
    uses, with letter-spacing tuned to match DejaVu Bold's metrics
    visually (CSS letter-spacing ≈ 0.06 em looks right against the
    system sans).
  - Paint the 300-bar FFT using `draw_fft`'s exact algorithm:
    pixel-aligned bar bounds (`floor`/`round` per side so adjacent
    bars share an exact boundary, no AA gap), bar height
    `v · area_h` with a 1 px floor, alpha
    `0x33 + (0xFF − 0x33) · v` over the accent RGB, and a dim
    horizontal floor line at the bottom. Use `ctx.fillRect` per bar
    on a `Path2D`-less hot loop; with 300 bars at 30 fps the per-
    frame cost is ~9 k `fillRect` calls which Chromium / Firefox /
    WebKit all handle comfortably above 60 fps on a 600 × 150
    surface. Rationale: this is the visible product — fidelity here
    is the whole point.

- [ ] Task 5. **Draw the lower-half "dictated text" surface.** A
  separate rounded panel (or a continuation of the same panel if
  the design lead prefers one tall card) in the bottom 600 × 150,
  carrying the scripted caption. Behaviour:
  - A short loop of 3 – 5 sentences chosen as the commercial copy
    (e.g. "Push to talk. Speak naturally. Fono types it for you.
    Fully offline, fully yours.") stored as a JS array.
  - A cursor advances through the current sentence; new characters
    appear only on virtual-audio frames where the synthetic
    envelope exceeds a threshold, so typing visibly pauses during
    the synthesised silent gaps from Task 2 — making the link
    between "audio activity in the top half" and "text appearing
    in the bottom half" feel causal rather than scripted.
  - Between sentences, briefly clear the text (or fade out) and
    show "POLISHING" on the upper-panel status label for ~500 ms
    using the amber `#E0A040` accent from `accent_color`'s
    `Processing` arm — sells the cleanup-LLM half of the product.
  - Then resume with the next sentence in the rotation. The whole
    loop should be ~25 – 35 s so a visitor sees at least one full
    cycle without it feeling too long.
  - Text style: `#ECECF1` body at ~20 px, with the current sentence
    growing in place and the previous one fading to `#AAAAB2`
    above it (matching the real overlay's transcript palette in
    `crates/fono-overlay/src/real.rs:252-253`).

- [ ] Task 6. **Drive everything from a single `requestAnimationFrame`
  loop with a virtual clock.** One scheduler:
  1. Accumulates real elapsed time into `virtualMs`.
  2. Generates enough new 16 kHz samples to advance the ring buffer
     by `(virtualMs − lastSampleMs)` worth.
  3. Every 50 virtual ms: re-runs Task 3's FFT pipeline and stores
     the new 300-bin frame.
  4. Every animation frame: redraws both halves using the most
     recent bin frame and the current text-cursor position.

  The loop pauses (skips sample / FFT work) when the page tab is
  hidden (via `document.visibilityState`) and resumes cleanly,
  preserving the deterministic virtual-clock position so the loop
  picks up where it left off. Rationale: keeps CPU off when nobody
  is looking, and stops the loop from "fast-forwarding" through
  hours of silence when a visitor switches back to the tab after
  lunch.

- [ ] Task 7. **HiDPI / responsive handling.** Read
  `window.devicePixelRatio`, size the canvas backing store at
  `600 × dpr` by `300 × dpr` while keeping the CSS box at
  600 × 300, and scale the 2D context by `dpr` so bars and text
  stay crisp on Retina / 4K displays. Clamp `dpr` to ≤ 2 so a
  visitor on a 3× phone doesn't pay for an 1800 × 900 backing
  store on a hero animation. Document the canvas as
  `aspect-ratio: 2 / 1` in CSS so the site can shrink it
  proportionally on narrow viewports.

- [ ] Task 8. **Pause / play affordance and reduced-motion respect.**
  Honour `prefers-reduced-motion: reduce` — when true, freeze on a
  representative frame (peak spectrum + first sentence fully
  rendered) instead of animating, and surface a small "play" pill
  in the corner so visitors who *want* the motion can opt in.
  Rationale: a 30-fps spectrum is high-motion content; respecting
  the OS hint is both a usability win and table-stakes for an
  accessibility-conscious project.

- [ ] Task 9. **Performance budget verification.** Profile in Chrome,
  Firefox, and Safari at 1× and 2× DPR; verify the demo holds
  60 fps with main-thread time under 6 ms per frame on a mid-range
  laptop (target machine: Intel i5-8xxx-class). If the radix-2 FFT
  shows up hot, fall back to FFT size 2048 with the same bin
  mapping (acceptable visual trade-off; freq resolution halves but
  the 300 display bins still average cleanly). Rationale: this
  animation runs on the front page; janky scroll kills hero
  conversions.

- [ ] Task 10. **Package the demo as a single self-contained snippet.**
  Deliverable is one `fono-demo.js` plus a minimal
  `fono-demo.html` (or a single `fono-demo.html` that inlines the
  script). No npm deps, no bundler, no module loader — a plain
  `<script defer src="/fono-demo.js"></script>` plus
  `<canvas id="fono-demo" ...>` works. Target ≤ 12 KB minified +
  gzipped. Ship under the same GPL-3.0 header as the rest of the
  project so anyone can fork the demo. Drop the files in the site
  repo; cross-link from this repo's `README.md` (existing "site"
  / "demo" link) so the source is discoverable.

- [ ] Task 11. **Visual regression check against a real recording.**
  Run Fono locally with `[overlay].style = "fft"` and capture a 5 s
  screen recording of the actual overlay at the same accent colour.
  Place a frame from the recording next to a frame from the JS
  demo and confirm: panel BG matches, accent stripe matches,
  status font weight & position match, bar palette & density
  match, floor line is present. File anything that disagrees as a
  follow-up issue. Rationale: the demo loses all marketing value
  the moment a visitor installs Fono and notices the website
  looked different.

## Verification Criteria

- Demo renders inside a 600 × 300 canvas with the FFT visualisation
  in the top half and animated dictated text in the bottom half,
  with no microphone prompt, no autoplay block, and no network
  requests after initial load.
- A side-by-side comparison with a real Fono FFT-style overlay
  recording shows: identical accent colours
  (`#637AFF` / `#E05454` / `#E0A040`), identical panel charcoal
  (`rgba(23, 23, 27, 0.80)`), identical bar alpha curve
  (`0x33 → 0xFF` linear over `v`), 300 bars across the spectrum
  area, and a visible dim floor line at the bottom of the bar area.
- Synthetic spectrum stays within 0 – 3 kHz, frames update at 20 Hz,
  and the dB normalisation maps the synth's loudest harmonic to
  ~0.9 and silence to ~0.0 — i.e. the visual dynamic range
  matches the real overlay.
- Text cursor only advances on frames where the synthetic envelope
  exceeds the activity threshold, so silent gaps in the spectrum
  visibly stall the typing.
- `prefers-reduced-motion: reduce` freezes the animation on a
  representative still and exposes an opt-in "play" affordance.
- Loop holds ≥ 58 fps on a 2019-era laptop in Chromium / Firefox /
  WebKit with the tab focused; CPU drops to ~0 % when the tab is
  hidden.
- Final asset is one HTML snippet + one JS file totalling ≤ 12 KB
  gzipped, with a GPL-3.0 header.

## Potential Risks and Mitigations

1. **Synthetic spectrum looks artificial.**
   Mitigation: use formant-shaped additive synthesis (Task 2) plus
   a noise floor and a syllable envelope; calibrate against a side-
   by-side recording (Task 11) and iterate on F0 range, formant
   amplitudes, and envelope rate until the spectrum reads as
   "voice" rather than "tone".
2. **Radix-2 FFT in JS is too slow on low-end devices.**
   Mitigation: pre-fallback path at FFT size 2048 (Task 9); also
   gate computation behind the 50 ms virtual tick so animation
   frames between ticks are cheap redraws of the cached bin frame.
3. **300 × `fillRect` per frame triggers Canvas slow paths in some
   browsers.**
   Mitigation: batch into a single `Path2D` of rectangles per
   colour bucket (the alpha is the only thing that changes per bar,
   so group by quantised alpha into ~8 buckets and call `fill`
   once per bucket).
4. **Site CSS pipeline differs from Fono's font (DejaVu Bold).**
   Mitigation: the status label is ~7 characters; pick a near-match
   system stack (`-apple-system, "Segoe UI", "DejaVu Sans Bold",
   sans-serif`) with letter-spacing tuned by eye against a
   reference screenshot. Do not ship a webfont — adds weight and
   privacy concerns.
5. **Visitors expect the demo to be interactive (click to record).**
   Mitigation: keep the demo non-interactive but add a subtle
   "Fono runs locally on your machine — try it →" call-to-action
   below the canvas that links to the install page. Avoids the
   complexity (and privacy footgun) of a real `getUserMedia` mic
   prompt on the hero.
6. **Demo drifts out of sync with overlay code over time.**
   Mitigation: leave a short comment block at the top of
   `fono-demo.js` citing the exact source files and constants
   (`session.rs` and `real.rs` line ranges from the references
   section above) so future overlay tweaks know to update the
   demo. Optionally add a follow-up roadmap item to regenerate
   the demo's reference screenshot whenever `WaveformStyle::Fft`
   constants change.

## Alternative Approaches

1. **Use `AudioContext` + `AnalyserNode` instead of a hand-rolled
   FFT.** Trade-offs: smaller code (~3 KB saved), but loses control
   over window, dB curve, and bin mapping — `AnalyserNode` uses
   Blackman by default and exposes its own dB clamps. Visual would
   diverge from the real Fono spectrum in subtle ways (different
   peak shape, different dynamic range). Reject for fidelity
   reasons; we want the demo to *be* Fono visually, not just
   "another spectrum analyser".
2. **Pre-render a looping GIF / MP4 of the real overlay and embed
   it as a `<video>`.** Trade-offs: zero JS, zero CPU, perfect
   fidelity to whatever was captured. Costs: file weight
   (≥ 1 MB for a 25 s loop at acceptable quality), no responsive
   re-rendering at HiDPI, no live state changes (e.g. swapping
   accent between RECORDING / LIVE / POLISHING), can't synchronise
   the typing caption to the audio envelope frame-accurately, and
   feels less "alive" than a real Canvas animation. Reject as the
   hero treatment; acceptable as the `<noscript>` fallback PNG /
   GIF (Task 1).
3. **Capture real PCM from a Fono session, ship the samples as a
   compressed blob, and play it back through the JS FFT.**
   Trade-offs: highest realism (real voice, real formants), but
   ships voice data with the page (privacy optics — even the
   developer's own voice — and an extra 50 – 200 KB asset) and
   creates an awkward "but I don't want to hear it" / autoplay
   policy interaction. The synthetic path (Task 2) gets 90 % of
   the realism with none of those drawbacks. Keep as an in-house
   reference recording for Task 11's calibration, but do not ship
   PCM to the site.
4. **WebGL shader instead of 2D Canvas.** Trade-offs: arguably
   smoother bar transitions and trivially supports glow / bloom
   effects if marketing later wants them. Costs: triples the
   implementation budget, fragile across mobile GPUs, and the
   current overlay deliberately uses pixel-aligned 2D rects with
   no glow — going GLSL would *diverge* from the product. Reject;
   2D Canvas matches what users actually see on their desktop.
5. **Render a Heatmap / Oscilloscope / Bars demo alongside the FFT
   one.** Trade-offs: showcases more of the product surface and
   sells the "five waveform styles" feature. Costs: 3 – 4× the
   code, 3 – 4× the calibration effort, and a busier hero. Keep
   the first ship narrow to FFT; once the FFT demo is stable, file
   a follow-up to swap styles on a slow rotation (every ~20 s) or
   on hover.
