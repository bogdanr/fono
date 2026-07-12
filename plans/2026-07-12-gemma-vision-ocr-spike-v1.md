# Gemma Vision Research (local screen OCR + description)

## Objective

Investigate whether Fono's **existing default local model** (`gemma-4-e2b`, a
Gemma 3n-class checkpoint) can, running **on the same CPU it already uses**,
take a screenshot and return an **OCR transcript and/or a plain-language
description of what is on screen** — feeding the F8 voice assistant a *local*
equivalent of the cloud "see your screen" feature shipped in v0.9.1. This is a
**low-priority, spike-first research project**: it produces real measurements
and a go/no-go memo before any shipping code is written. Nothing here blocks or
reorders the current roadmap.

## Why now / why at all

- The default local LLM (`crates/fono-core/src/config.rs:711`,
  `DEFAULT_POLISH_LOCAL_MODEL = "gemma-4-e2b"`) is a Gemma 3n-class model whose
  upstream checkpoint already ships a **vision tower** (MobileNet-V5-class
  encoder, 256 image tokens per tile) and an audio encoder. We fetch and load
  only the **text GGUF** today — the multimodal projector (`mmproj`) is never
  downloaded, so the capability is latent and unused.
- The cloud assistant can already look at the screen when the user points at
  something (v0.9.1, ROADMAP "See your screen, dictate in any language"). A
  **local** counterpart — no cloud, no key, on the model already resident —
  would extend that to the offline / privacy-first path Fono is built around.
- The motivating value is **"describe what's on my screen" for the F8
  assistant**, not verbatim document OCR (a dedicated OCR engine beats a small
  multimodal LLM on dense text — see Candidate workloads). Battery/latency is
  not the point; capability-on-the-local-path is.

## Reference baseline (measured, dev machine — text path)

We have **no multimodal numbers yet** — the figures below are the real *text*
baseline the spike must extend. Same box as the LLM-speed blog work
(`plans/2026-06-08-fono-llm-speed-blog-post-v1.md`): `gemma-4-e2b`, Q4_0 GGUF,
8 threads, ctx 4096, 8-core CPU (dev-nimblex):

- **Decode:** ~22–26 tok/s (`crates/fono-core/src/llama_backend.rs:117-119`).
- **Prefill:** cold ~330-token assistant prompt ≈ 2.9 s
  (`plans/2026-06-08-fono-llm-speed-blog-post-v1.md:171`) → ~115 tok/s prefill.

Extrapolated (estimate only — Phase A must measure) single-tile screenshot turn:

| Stage | What happens | Estimate (8-core CPU) |
|-------|--------------|-----------------------|
| Vision encode | run the image encoder once (softest estimate) | ~1–3 s |
| Prefill | 256 image tokens + prompt through the transformer | ~2.5–5 s |
| Decode | generate answer at ~22 tok/s | length-dependent |

So a short "describe the screen" (~100 tokens out) plausibly lands in **~8–15 s**
end-to-end; a full verbatim OCR dump (~400–600 tokens out) in **~25–40 s**.
Roughly **double** on a 4-core machine.

Design implications: screenshots are close to the **worst case** for a small
multimodal model — wide aspect + dense small text. Squashing 1920×1080 to a
single 768-px tile makes UI text unreadable; keeping text legible needs Gemma
3n **pan-and-scan** tiling, which multiplies vision tokens (2–4 tiles =
512–1024+ tokens) and pushes prefill to ~5–10 s. "Fast" and "reads the small
text" pull against each other.

## Non-negotiable constraints

- **Default ship binary is unchanged.** The multimodal runtime (llama.cpp's
  `libmtmd`, formerly `clip.cpp`) must **not** be linked into the default
  25 MiB static build (ADR 0022 size gate, `./tests/check.sh --size-budget`).
  The vision path is an **opt-in build variant** (mirroring the `accel-*`
  feature-flag pattern and the NPU spike's `openvino` variant).
- **No new-to-project dependency** in the default graph without sign-off
  (AGENTS.md). The **binding gap is the first thing to settle**: our pinned
  `llama-cpp-2 = 0.1.150` (`Cargo.lock:2878-2879`) almost certainly does **not**
  expose the `mtmd` multimodal API. Phase A must determine whether a bindings
  bump, a `-sys` shim, or an upstream contribution is required — and its size
  cost in the opt-in variant.
- **Detect-and-fall-back, always.** In the opt-in variant, the vision path is
  offered only when the `mmproj` companion file for the active model is present;
  otherwise Fono behaves exactly as today (text-only assistant). No crash, no
  startup slowdown when the projector is absent.
- **Extra download is separate + opt-in.** The `mmproj` projector (~a few
  hundred MB) is fetched only on explicit opt-in, never bundled, never part of
  the default first-run footprint (ADR 0004).
- **License:** the multimodal artifact and its `mmproj` must stay on the same
  Apache-2.0 Gemma 3n QAT/GGUF line already blessed for defaults
  (`docs/decisions/0004-default-models.md:107-113`). No non-OSI variant.

## Candidate workloads (ranked by fit)

| Rank | Workload | Fit | Notes |
|------|----------|-----|-------|
| 1 | **"Describe what's on screen"** for the F8 assistant | good | genuinely new local capability; ~8–15 s felt latency; Gemma 3n is strong at scene/UI description |
| 2 | **Targeted read** ("what does this dialog say?", small region) | fair | single tile, short output → fastest, most reliable |
| — | **Full verbatim page OCR** | **poor — excluded from v1** | slow (~25–40 s), quality-risky (paraphrases, drops lines, no bounding boxes). A dedicated OCR engine (Tesseract/PaddleOCR) wins; Gemma only for the explain/summarise layer over its output |
| — | **Audio-in via Gemma's audio encoder** | **excluded** | redundant with the purpose-built STT stack (whisper/Zipformer, ADR 0004); much slower and heavier for no accuracy win on dictation |

## Phase A — Investigation spike (NO Fono changes)

External harnesses on a scratch checkout; zero shipping-code changes.

- [ ] Task A1. **Binding-gap decision.** Determine whether `mtmd` is reachable:
      does any `llama-cpp-2` release expose the multimodal API, or is a `-sys`
      shim / vendored `libmtmd` call required? Document the exact recipe and the
      **binary-size delta** of linking `libmtmd` + image preprocessing into an
      opt-in variant. This gates everything else.
- [ ] Task A2. **One real vision-encode + prefill measurement.** With a
      stock llama.cpp `mtmd` build + the Gemma 3n `mmproj`, feed a single
      768-px screenshot tile and measure: image-encode wall-clock, image-token
      prefill wall-clock, and tok/s decode — turning the estimates above into
      real numbers. Headline experiment.
- [ ] Task A3. **Resolution / pan-and-scan curve.** Measure encode + prefill for
      (a) a downscaled single tile and (b) pan-and-scan tiling of a real
      1920×1080 and 2560×1440 desktop screenshot; record tile count, total
      vision tokens, and total latency. Establishes the "legible vs fast"
      trade-off with data.
- [ ] Task A4. **Quality probe.** On a fixed set of representative screenshots
      (terminal + error, IDE, browser, settings dialog), score description
      usefulness and OCR fidelity qualitatively. Confirm the v1 scope line:
      description = good, verbatim OCR = out.
- [ ] Task A5. **Memory + contention.** Confirm resident-set impact of loading
      the `mmproj` alongside the ~3.2 GB text weights, and whether vision-encode
      contends with a concurrent polish/assistant decode (shared model +
      backend, `crates/fono-core/src/llama_backend.rs`).
- [ ] Task A6. **Artifact + license audit.** Confirm an Apache-2.0
      static-shape `mmproj` for the default Gemma 3n tier exists / is producible
      (ADR 0004), and estimate the extra opt-in download size.
- [ ] Task A7. Write the **go/no-go memo** into this file: real
      encode/prefill/decode numbers, felt end-to-end latency for the two "go"
      workloads, resolution trade-off curve, quality verdict, `mmproj` download
      size, opt-in-variant size delta, and integration effort. Suggested "go"
      bar: **description turn ≤ ~15 s end-to-end** on the reference box with
      **zero impact on the default binary** and a legible-text tiling strategy
      that stays under that bound.

## Phase B — Integration (gated on a Phase A "go")

Only proceeds if Phase A clears the bar; description-of-screen first.

- [ ] Task B1. Add an **opt-in build variant / feature flag** (e.g.
      `vision-local`) that links the `mtmd` multimodal path. Prove the default
      build's binary size and `NEEDED` allowlist are byte-for-byte unchanged
      (`./tests/check.sh --size-budget`).
- [ ] Task B2. **Opt-in `mmproj` download** wired into the model-fetch flow with
      a license notice-on-download; detect-and-fall-back when absent.
- [ ] Task B3. Route an F8 assistant "look at my screen" turn through the local
      vision path (screenshot capture → tile/pan-and-scan → encode → prefill →
      decode), reusing the existing screen-capture plumbing from the v0.9.1
      cloud path where possible.
- [ ] Task B4. Surface the active path in `hardware_acceleration_summary()` /
      `fono doctor` and document the opt-in variant + `mmproj` in
      `docs/providers.md`.
- [ ] Task B5. Author an **ADR** capturing the decision, the opt-in architecture,
      the size-budget guarantee, the description-not-OCR scope line, and the
      detect-and-fallback contract.

## Out of scope

- **Verbatim page OCR** as a first-class feature (excluded above; dedicated OCR
  engine is the right tool if this is ever wanted).
- **Audio-in via Gemma's audio encoder** (excluded; STT stack already owns it).
- **Making the multimodal runtime part of the default binary** (forbidden —
  opt-in only, same rule as the NPU spike).

## Open questions

- Is `mtmd` reachable through `llama-cpp-2` at all, or does the opt-in variant
  need a `-sys` shim / vendored build? (Task A1 decides whether this is even
  practical for us.)
- Is the felt latency for "describe my screen" (~8–15 s estimated) acceptable
  for an on-demand assistant query, or does it kill the UX? (Task A2/A7.)
- Does the legible-text tiling cost blow the latency bar on real desktop
  resolutions? (Task A3.)
- Is a local screen-description worth an opt-in system-scale build variant and a
  few-hundred-MB `mmproj` download, given the cloud path already exists? (A7.)
