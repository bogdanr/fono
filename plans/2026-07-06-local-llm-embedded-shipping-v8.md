# Local LLM Upgrade — Embedded-Only Shipping Plan

## Objective

Ship the measured headline win of the 2026-07 technique exploration — MoE-class local models
that are **both smarter and 2–3× faster** than the dense alternative on laptop-class hardware —
through Fono's **existing embedded `llama-cpp-2` path**, with ~zero binary growth, no sidecar
artifacts, and full resilience to model churn.

v8 changes vs v7: **Phase D executed and decided.** The expert-offload ("ds4 line") measurement
is complete for both candidate MoE models across a three-rung ladder (experts on GPU → experts
on CPU in RAM → experts faulting from NVMe under an 8 GB cgroup cap). Result: **the ds4 line is
parked with numbers.** The decisive finding reframes the whole tier story and is folded into
Phase B below. Everything else from v7 stands.

Design constraints (ratified): single binary (cpu + gpu variants already shipped by CI/release);
one-maintainer project — no additional downloadable runtime artifacts; every technique must
justify its cost via the intelligence×speed trade-off; model selection adapts to the host
machine; models change every couple of months — the design must not require code per model.

## Measured evidence base (Intel Lunar Lake iGPU, Vulkan/UMA, 30 GB RAM, Samsung NVMe 7.6 GB/s)

### Models that fit on the GPU (from v4/v5)

| Model | Shape | EN/RO/CODE tok/s | TTFT | Verdict |
|---|---|---|---|---|
| Gemma 4 E2B (current default) | dense 2B-class | fast, weakest quality (RO factual 62.5%) | ~0.3 s | stays as floor tier |
| Gemma 4 12B | dense | 12.0 / 8.9 / 8.9 | 0.6–0.8 s | dominated by MoE |
| Gemma 4 26B-A4B Q4_0 (14.4 GB) | MoE ~4B active | 24.3 / 18.7 / 20.3 | 0.7–1.1 s | high-tier candidate |
| Qwen3.6-35B-A3B Q3_K_XL (17.2 GB) | MoE ~3B active | 21.3 / 17.6 / 18.5 | 0.9–1.8 s | high-tier candidate |

### Phase D — expert-offload ladder (NEW, 2026-07-06)

Decode tok/s (EN / RO / CODE), warm; cold-load seconds; peak disk read per token:

| Rung | Where experts run | Gemma 26B-A4B | Qwen 35B-A3B | cold load | disk R/tok |
|---|---|---|---|---|---|
| **R1** | iGPU (full offload) | **20.4 / 19.8 / 20.4** | **21.1 / 20.9 / 20.0** | 21 s / 11 s | ~0 |
| **R2** | CPU, fully in RAM | 8.1 / 9.4 / 8.0 | 6.4 / 7.5 / 6.9 | 7 s / 7 s | ~0 |
| **R3** | CPU, 8 GB cap → NVMe faulting | 7.1 / 6.7 / 5.9 | 7.9 / 6.9 / 5.1 | 8 s / 9 s | 25–137 MB |

**The decisive result: R2 ≈ R3.** Forcing experts to fault from NVMe under an 8 GB cap is
*essentially the same speed* as keeping them in RAM. The Samsung NVMe (7.6 GB/s sequential)
keeps up with expert paging — even the code prompt's ~100–137 MB/token of faulting only cost a
further ~1–2 tok/s. **Disk is not the bottleneck.**

**What IS the bottleneck: expert *compute location*.** Moving experts off the iGPU collapses
decode ~20 → ~7 tok/s (≈3×), and that collapse happens the moment experts leave the GPU —
independent of whether they then live in RAM or on disk. On this UMA machine the experts sit in
the *same physical memory* in R1 and R2; the only thing that changed is whether the iGPU or the
CPU does the expert matmuls. The iGPU is ~3× faster at them. This is a compute-placement result,
not a memory-placement result.

**Consequence for the ds4 thesis:** ds4's premise (NVMe streaming of experts is viable) is
literally *confirmed* — the disk keeps up. But it is **pointless for our goal**, because the
speed you unlock by streaming experts you can't fit on the GPU is only ~7 tok/s — barely above
the dense Gemma 4 12B (8–12 tok/s) that already fits, and *below* the ~10–15 tok/s comfort floor
for real-time TTS streaming. There is no "big brain at usable speed" prize on the far side of
the GPU memory budget on this class of hardware; there is only "big brain at dense-12B speed."

Gate outcome (threshold was ≥8 tok/s ⇒ build; ≤4 ⇒ park): both models land ~5–8 tok/s,
predominantly **below 8**, so **Phase D is parked** — but the more important takeaway is the
*reason*: even at the ceiling it wouldn't have been worth the engineering, because the win over
an already-fitting dense model is marginal.

### Additional captured signals (relevant later)

- **Cold-load time** is dominated by GPU upload: R1 Gemma 21 s / Qwen 11 s (weights → iGPU),
  vs ~7–9 s for CPU-resident rungs. First-use latency budgeting must account for this.
- **Warm TTFT stays low** even under NVMe pressure (100–400 ms EN), because the prompt-eval
  path and hot pages are cached; the pain of faulting shows up in *decode*, not first token.
- **No memory leaks** across all runs (RSS stable within each config).
- **Prompt-eval (prefill)** stays healthy off-GPU (15–30 tok/s), so the degradation is
  specifically the per-token expert read+compute in the decode loop.

Rejected with measurements (win doesn't justify embedding cost): all speculative decoding
(EAGLE-3 regressed Romanian −25% at 7% acceptance; MTP is server-layer; n-gram is a net loss on
fresh output) **and now expert offload beyond the GPU budget** (≈7 tok/s, no advantage over a
fitting dense model). Revisit speculation only if upstream exposes it via the core C API;
revisit offload only if a future iGPU/NPU makes off-GPU expert compute fast.

## Implementation Plan

### Phase A — prerequisite checks — **COMPLETE**

- [x] Task A1. Pinned `llama-cpp-2` architecture support for Gemma 4 MoE and Qwen3.6
      (`qwen35moe`, `GATED_DELTA_NET`) — verified during the exploration.
- [x] Task A2. Core C API reachability of `tensor_buft_overrides` / `use_mmap` confirmed
      (`llama.h:299,323`). Note: no longer on the critical path — Phase D parked.

### Phase B — model introspection + MoE tiers (the headline feature)

- [ ] Task B1. Build a GGUF introspection step in the embedded backend: on model
      selection/load, read from the file header — `{arch}.expert_count` and
      `expert_used_count` (dense vs MoE, active-parameter class), layer/head/dim counts
      (KV-cache cost prediction), file size + quant type (working-set estimate), and
      `tokenizer.chat_template` (thinking-format detection). Prefer the bindings' existing
      metadata APIs post-load; add a lightweight pre-load header parse only if tier gating
      needs the answer before committing to a load.
- [ ] Task B2. Derive technique application from introspection, not from lists: template
      contains thinking markers ⇒ matching prevention + scrub (Phase C); KV dims + requested
      ctx ⇒ memory budget check. Unknown/missing metadata degrades to today's conservative
      defaults — never a hard failure.
- [ ] Task B3. Keep the registry (`crates/fono-polish/src/registry.rs`) as curated
      distribution only: add the two MoE candidate rows (Gemma 4 26B-A4B QAT Q4_0;
      Qwen3.6-35B-A3B UD-Q3_K_XL) with pinned SHA256 + Apache-2.0 licence (both verified
      against ADR 0004). No shape/technique metadata in the table.
- [ ] Task B4. **Tier selection — now GPU-budget-driven (revised per Phase D):** extend
      `LocalTier` mapping (`crates/fono-core/src/hwcheck.rs`) and the wizard so an MoE model is
      offered **only when its full weights fit within the host's usable GPU/UMA budget** (the
      condition under which we measured the 2–3× win). The runnability check is
      *introspected total weight size vs available GPU budget*, NOT "fits in RAM via expert
      streaming" — Phase D proved off-GPU experts give only dense-12B speed, so we must not
      advertise an oversized MoE as a fast tier. When an MoE does not fit the GPU budget,
      prefer a fitting dense model of the next tier down rather than a slow expert-offload
      config. E2B stays the universal floor. Wizard nudges iGPU-capable hosts toward the gpu
      binary variant.
- [ ] Task B5. ADR recording: the MoE-tier decision, the introspection-over-metadata design,
      the **Phase D expert-offload rejection with the compute-location finding**, and the
      measured evidence, per docs/decisions convention.

### Phase C — correctness hardening (required by any modern model)

- [ ] Task C1. Template-driven thinking prevention: extend the embedded prompt-template
      handling (`crates/fono-polish/src/llama_local.rs:898-996`) to select the prevention
      strategy from the *introspected* chat template (Qwen `<think>` seeding, Gemma 4 channel
      format) instead of model-name matching.
- [ ] Task C2. Output-side scrub for known thinking-marker families
      (`<think>…</think>`, `<|channel>thought…`) in the polish/assistant response path —
      defense-in-depth for templates the prevention layer doesn't recognise; leaked markers
      would be spoken aloud by TTS today.
- [ ] Task C3. Enable KV-cache Q8_0 + flash-attention in embedded context params where the
      bindings expose them, gated by introspected KV dims + requested ctx. Labeled as a
      long-context enabler (measured: no speed effect at ctx ≤4096; ~47% KV saving matters at
      ctx ≥8k or multi-slot).

### Phase D — the ds4 line (expert offload) — **PARKED (measured 2026-07-06)**

- [x] Task D1. Expert-offload ladder run for both MoE models (R1 GPU / R2 CPU-RAM / R3
      NVMe-8G-cap), instrumented for tok/s, TTFT, cold-load, major page faults, disk bytes,
      peak RSS, cgroup memory. Results in the evidence table above.
- [x] Task D2. Decision: **park.** Off-GPU expert decode is ~5–8 tok/s for both models,
      below the ≥8 gate and — decisively — no better than a dense model that already fits the
      GPU. NVMe streaming works (disk keeps up; R2≈R3) but buys nothing because the bottleneck
      is CPU expert *compute*, not expert *transfer*. No `tensor_buft_overrides` passthrough,
      no oversized-MoE runnability logic. Revisit only if off-GPU expert compute gets fast
      (future NPU/iGPU) or upstream changes the economics.

## Explicitly out of scope (tracked elsewhere)

- Grammar-constrained tool calls + tool-call prompt tuning → future tool-use plan.
- Concurrent TTS / ORT session sharing → separate audio-stack design task (decode-thread
  contention makes it non-trivial).
- Speculative decoding in any form → rejected with measurements; revisit on upstream C-API
  exposure.
- Managed `llama-server` sidecar → rejected: violates single-binary/one-maintainer constraint.
- Expert offload beyond the GPU budget (ds4 line) → parked with measurements (Phase D).

## Verification Criteria

- `fono` (cpu variant) stays within the 25 MiB size budget; size-budget gate green.
- On a 16 GB+ iGPU machine with the gpu binary, the wizard offers an MoE-tier model **only when
  it fits the GPU budget**, and the assistant reaches ≥15 tok/s decode with ≤1.2 s TTFT at
  ctx 2048.
- An MoE model that does *not* fit the GPU budget is **not** offered as a fast tier (Phase D
  finding); a fitting dense model is preferred instead.
- On a 4–8 GB machine, behaviour unchanged (E2B floor intact).
- A GGUF absent from the registry, supplied via manual config, gets correct technique
  treatment (MoE detection, thinking handling, memory gating) purely from introspection —
  covered by a unit/integration test with a crafted header.
- No thinking markers reach TTS or injected text for Qwen-think and Gemma-channel families
  (unit tests over scrub + template selection).
- Adding a curated model touches only the registry download table.

## Potential Risks and Mitigations

1. **GGUF metadata missing/nonstandard on some quantizer outputs** (community GGUFs vary).
   Mitigation: introspection degrades to conservative defaults (treat as dense, scrub-only
   thinking handling, file-size-as-working-set); never a hard failure.
2. **Chat-template sniffing misclassifies a thinking format.**
   Mitigation: the Phase C2 output scrub is the universal safety net; template detection only
   picks the *prevention* optimisation.
3. **Bindings lack a pre-load header-parse API** (metadata may only be readable post-load).
   Mitigation: tier gating can use a cheap load-with-mmap-then-inspect flow, or a ~200-line
   pure-Rust GGUF header reader (no new dependency — format is simple KV pairs).
4. **GPU-budget estimation is vendor/driver-dependent** (only Intel Lunar Lake UMA measured).
   The B4 "fits the GPU budget" check is the linchpin now; a wrong estimate either hides a
   usable MoE tier or advertises a slow one. Mitigation: conservative budget headroom, Vulkan
   probe gating, CPU fallback, and a first-run decode-speed self-check that can demote the tier
   if measured tok/s is below target.
5. **14–17 GB first-run downloads and long GPU cold-load (~11–21 s).**
   Mitigation: wizard states size/disk/first-load time up front; floor tier remains default
   when headroom is marginal; downloader already resumes + verifies SHA.

## Alternative Approaches

1. Static registry technique-metadata (v6 design) — simpler to implement but silently
   mis-serves unlisted models and adds per-model maintenance; superseded by introspection.
2. Managed `llama-server` child process — rejected on single-binary/maintenance grounds.
3. Vendored speculation — rejected on win/cost ratio (+30% code-only vs permanent fork).
4. Expert streaming from NVMe for oversized MoE (ds4) — rejected on measured speed
   (Phase D: ~7 tok/s, no advantage over a fitting dense model; bottleneck is off-GPU compute,
   not disk).
