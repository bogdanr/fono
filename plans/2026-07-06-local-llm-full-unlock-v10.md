# The Full Unlock — Big Models, Small Footprint, GPU Speed (final roadmap)

## Objective

The end state, in one sentence: **any laptop runs the biggest MoE brain its SSD can hold, at
GPU speed, in whatever RAM other apps leave free — and Fono picks the right mode automatically.**

This is the consolidated "final plan" that sequences everything measured and designed across
v4–v9 into four stages, each independently shippable, each raising the ceiling:

| Stage | What the user gets | Decode speed | Ships as |
|---|---|---|---|
| 1. Glue | 26–35B MoE brains today: full speed when they fit, streaming when they don't | 20–26 tok/s fit / 5–8 streamed | Fono release |
| 2. Shrink | Same brains fit on 16 GB machines at full speed | 20+ tok/s on 16 GB | registry rows |
| 3. **GPU expert cache** | Streaming at near-GPU speed — the mega unlock | **~15–20 tok/s streamed** | llama.cpp PR |
| 4. Horizon | True GPU paging (Xe SVM etc.) | — | watch upstream |

Evidence base: all numbers measured on Intel Lunar Lake (Arc 140V, Vulkan/UMA, 30 GB, NVMe
7.6 GB/s) in `plans/2026-07-06-local-llm-embedded-shipping-v9.md`. Design constraints ratified:
single binary, one maintainer, no sidecar artifacts, model-churn-proof (introspection, not
per-model code).

## Why this sequencing (the physics recap)

- MoE experts on the **iGPU**: ~20 tok/s, 1.08 J/token. Same experts on **CPU**: ~7–8 tok/s,
  2.43 J/token — *even when fully in RAM*. The 3× penalty is compute placement, not memory.
- The **NVMe streaming itself is nearly free** (~1 tok/s; R2 ≈ R3): the page cache + a fast SSD
  absorb expert faults. ds4's premise holds.
- Therefore the mega unlock = **keep experts evictable/file-backed AND compute them on the GPU**.
  Nothing in the hardware forbids this on UMA machines — only llama.cpp's placement rule
  ("pageable ⇒ CPU") does. That rule is Stage 3's target.

## Stage 1 — Ship the glue (Fono-only, ~1–2 weeks, probability ~90%)

Everything from v9 Phases B/C/D2, no upstream dependencies:

- [ ] 1.1 GGUF introspection (expert_count, dims, chat template, size) — the model file is the
      single source of truth; user-supplied GGUFs get correct treatment automatically.
- [ ] 1.2 Registry rows for Gemma 4 26B-A4B Q4_0 + Qwen3.6-35B-A3B UD-Q3_K_XL (download-only
      metadata; both Apache-2.0, ADR 0004 clean).
- [ ] 1.3 Tier logic: fits-GPU-budget ⇒ fast tier (Vulkan heap probe + headroom + first-run
      decode self-check); doesn't fit ⇒ **streaming mode** (`tensor_buft_overrides` = experts
      to CPU/mmap, no warmup, mmap on) for latency-tolerant workloads; never stream when it
      fits (−2.4× speed, −2.25× energy, audio-core contention).
- [ ] 1.4 Thinking-format prevention from introspected template + output scrub (the TTS leak).
- [ ] 1.5 KV-Q8 + flash-attention context flags (long-context enabler, verified on Vulkan).
- [ ] 1.6 Optional "leave N GB for other apps" knob (`memory.high` on Fono's scope, Linux).
- [ ] 1.7 ADR: MoE tiers, introspection design, streaming mode, measured evidence.

Outcome: today's hardware ceiling fully exploited; the 2–3× smarter-and-faster headline ships.

## Stage 2 — Shrink to fit (measurement + registry, days, probability ~60%)

- [ ] 2.1 Measure a 2-bit-routed-expert (asymmetric imatrix) GGUF of Gemma 4 26B-A4B: target
      ~8–9 GB total (fits 16 GB machines' GPU budget with headroom for Firefox).
- [ ] 2.2 Quality gate: existing factual/tool fixtures (EN+RO) — 2-bit experts must not
      regress below the dense-12B quality bar; ds4's evidence says they won't, ours must confirm.
- [ ] 2.3 If green: registry row + tier mapping. Shrinking beats streaming wherever quality
      holds; streaming covers the rest.

Outcome: full 20 tok/s MoE speed reaches the 16 GB laptop majority, not just 32 GB machines.

## Stage 3 — The mega unlock: GPU expert cache (llama.cpp patch, the real work)

**Design (ds4's architecture, done properly inside ggml/Vulkan):** a fixed-budget, LRU
**expert cache in GPU-visible memory**. Expert matmuls *always* run on the iGPU. On cache miss,
the expert's weights are copied from the mmap'd GGUF (page cache → NVMe if cold) into a cache
slot; on hit, zero cost. Non-routed weights stay resident as today. The cache budget is a knob
(e.g. "4 GB of experts on GPU"), so resident footprint is bounded and the page cache behind it
remains evictable under external pressure — Firefox squeezes the *cache*, not the model.

Why the ceiling is ~15–20 tok/s: UMA memcpy runs at tens of GB/s; worst-case all-miss traffic is
~1.3–2 GB/token ⇒ ~60–100 ms/token added (≈ CPU-speed floor), but measured temporal locality
(R2≈R3) implies high hit rates ⇒ typical added cost ~5–15 ms/token on top of the 48 ms/token GPU
decode ⇒ **~15–20 tok/s streamed**. Degrades gracefully toward the floor as pressure rises.

- [ ] 3.1 **Feasibility spike (1–2 days, decisive):** hack a fixed host-visible Vulkan buffer
      pool into the MoE path on UMA; measure copy-in cost per expert and hit-rate on real
      generations (instrument expert IDs per token — we already know how to capture
      faults/token). Kill gate: if hit rate <70% or copy cost >30 ms/token, stop; Stage 1's
      CPU streaming remains the fallback and Stage 2 carries the load.
- [ ] 3.2 Upstream design issue first (llama.cpp discussion referencing ds4/DSpark lineage +
      spike numbers) — landing on an invited design beats a cold PR.
- [ ] 3.3 Implement behind a flag (e.g. `--moe-expert-cache <MiB>`): ggml-level expert-cache
      buffer type + Vulkan scheduling change; CPU fallback path untouched; non-UMA dGPUs work
      too (copies cross PCIe — still likely a win vs CPU compute).
- [ ] 3.4 PR upstream; if stalled, carry as a pinned patch in Fono's llama.cpp build (cost:
      rebase per bump — acceptable for one flag-gated file-local feature) while the PR ages.
- [ ] 3.5 Fono integration: streaming tier auto-uses the cache when the backend reports it;
      introspection sizes the cache budget from free-GPU-heap.

Effort: ~2–4 weeks engineering + upstream latency. Probability: spike ~80% informative;
end-to-end landing ~50% upstream, ~75% counting the carry-patch fallback. Expected value: it
converts every "doesn't fit" machine from 5–8 to ~15–20 tok/s — the single biggest remaining win.

## Stage 4 — Horizon (watch, don't build)

- Xe SVM mainlining (GPU page faults on system memory) → would let Stage 3's cache become true
  demand paging; Intel-only, SYCL-gated today.
- Upstream C-API speculation exposure → revisit MTP (+34% code) as a free layer on top.
- Small upstream QoL patches, opportunistic: selective mlock (pin non-routed weights only);
  temporal expert prefetch (`madvise(WILLNEED)` on previous-token experts).

## What the user experiences at each stage (the point of it all)

1. **Stage 1:** "My 32 GB laptop answers in Romanian, correctly, at conversation speed. My 8 GB
   machine still works exactly as before. My coding agent can use a 35B brain overnight."
2. **Stage 2:** "My 16 GB laptop got the big brain at full speed."
3. **Stage 3:** "I opened 40 browser tabs and my assistant got a bit slower instead of dying —
   and the big model runs at near-full speed even though it doesn't fit."

## Verification Criteria (cumulative)

- Size budget: `fono` cpu variant ≤ 25 MiB throughout (all stages are glue/config or live in
  the llama.cpp build, not new Rust dependencies).
- Stage 1: ≥15 tok/s + ≤1.2 s TTFT on fit-GPU MoE (16 GB+ iGPU, gpu binary); streaming mode
  engages only on doesn't-fit; E2B floor untouched; no thinking markers reach TTS; unlisted
  GGUFs handled via introspection (unit test with crafted header).
- Stage 2: 2-bit-expert model ≥ dense-12B quality bar on EN+RO fixtures; fits 16 GB GPU budget.
- Stage 3: spike numbers recorded either way; if shipped — streamed decode ≥2× CPU-streaming
  baseline on the same model/machine, graceful degradation under induced memory pressure.

## Risks

1. Stage 3 hit-rate assumption fails on some models/workloads → kill gate at the spike; Stages
   1–2 already shipped and independent.
2. GPU-budget probe wrong on non-Intel iGPUs → conservative headroom + first-run self-check
   demotion (Stage 1.3).
3. 2-bit-expert quality regression (Stage 2) → quality gate before any registry row.
4. Upstream rejects the expert-cache PR → flag-gated carry patch; feature stays Fono-buildable.
5. Model churn → all stages keyed on introspected shape, zero per-model code; next king model
   is a registry row.
