# Local LLM Upgrade — Embedded-Only Shipping Plan

## Objective

Ship the measured headline win of the 2026-07 technique exploration — MoE-class local models
that are **both smarter and faster** than the dense alternative on laptop-class hardware —
through Fono's **existing embedded `llama-cpp-2` path**, with ~zero binary growth, no sidecar
artifacts, and full resilience to model churn. **Plus** a secondary, latency-tolerant capability
(v9): running models *larger than a machine's RAM* via NVMe expert paging, for non-realtime
workloads and small-RAM hardware.

v9 changes vs v8 (**important correction**): the v8 conclusion that "expert offload is pointless"
was **too hasty** — it judged offload only against GPU speed for the *voice* workload. Two new
tests (full-GPU under a 4 GB cgroup cap; CPU+NVMe under a 4 GB cap, both models) revealed:
(1) the mechanism v8 called "the ds4 line failing" is actually a **real, working capability** for
latency-tolerant workloads and small-RAM devices; and (2) a subtle UMA/cgroup fact that reframes
what "RAM footprint" even means for a GPU-offloaded model. Details below. The voice-tier
conclusion is unchanged; the offload verdict flips from "parked" to "**conditional, workload- and
hardware-gated**."

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

### Full expert-offload matrix (Phase D + v9 corrections, 2026-07-06)

Decode tok/s (EN / RO / CODE), warm. "proc RAM" = process host RSS / cgroup-charged memory.

| Config | experts on | cgroup cap | Gemma 26B-A4B | Qwen 35B-A3B | proc RAM | disk R/tok | majflt/tok |
|---|---|---|---|---|---|---|---|
| **R1** | iGPU | none | 20.4 / 19.8 / 20.4 | 21.1 / 20.9 / 20.0 | ~0.2 GB* | ~0 | ~0 |
| **GPU+4G** | iGPU | 4 GB | **26.0 / 25.6 / 26.4** | **21.3 / 20.8 / 18.6** | ~0.2 GB* | ~0 | ~0 |
| **R2** | CPU | none | 8.1 / 9.4 / 8.0 | 6.4 / 7.5 / 6.9 | ~13 GB | ~0 | ~0 |
| **R3** | CPU | 8 GB | 7.1 / 6.7 / 5.9 | 7.9 / 6.9 / 5.1 | ~7.5 GB | 25–137 MB | 39–380 |
| **R4** | CPU | 4 GB | 4.3 / 5.3 / 3.7 | 4.2 / 4.5 / 2.9 | ~3.8 GB | 133–362 MB | 366–892 |

\* **The UMA/cgroup subtlety (key v9 finding).** In the GPU rows the *process* RSS is tiny
(~200 MB) and the 4 GB cgroup cap is not even approached — yet `free` showed **~24 GB physical
RAM used** during the run. Reason: GPU-offloaded weights are allocated through the graphics
driver and are **not charged to the process's memory cgroup**, but on a UMA machine that memory
is **still physical RAM**. So "GPU offload uses almost no RAM" is an illusion for coexistence
purposes: the 14–17 GB is still occupied, just invisible to per-process accounting. The cgroup
cap therefore does **not** simulate a small-RAM machine for the GPU path — only the CPU rows do.

### What the matrix actually says (three honest conclusions)

1. **The ~3× decode collapse is a compute-*location* effect, not a memory-location effect.**
   R1 vs R2: same physical memory (UMA), experts just move from iGPU compute to CPU compute →
   20 → ~7 tok/s. The iGPU is ~3× faster at the expert matmuls. Confirmed by R2 ≈ R3 (RAM vs
   NVMe makes almost no difference — the Samsung NVMe at 7.6 GB/s keeps up with paging).

2. **NVMe paging genuinely lets you run a model bigger than your RAM.** R3/R4 ran the 14–17 GB
   models inside an 8 GB / 4 GB physical footprint. At 8 GB: ~5–8 tok/s. At 4 GB: ~3–5 tok/s,
   with heavy thrashing (hundreds of major faults and 130–360 MB of disk reads *per token* —
   i.e. most of the hot-expert set is re-read every token because the working set (~8–13 GB)
   doesn't fit the cap). This is a **real capability**, not a failure — it just isn't a
   voice-latency capability.

3. **The only way to get full GPU speed AND a small *physical* footprint does not exist on this
   stack.** GPU device buffers are pinned physical RAM (not pageable to NVMe) and consume it
   regardless of cgroup accounting; CPU+mmap buffers are pageable (small footprint) but force
   slow CPU compute. You pick one: fast+large-RAM, or slow+small-RAM.

### Workload reframing (why v8 was too hasty)

| Workload | latency need | offload verdict |
|---|---|---|
| **Voice assistant (W1)** | ≥10–15 tok/s, TTFT <1 s | Offload useless. Model must fit the GPU budget. **v8 conclusion stands.** |
| **Tools / smart home (W2)** | turn <5 s | Borderline; 7 tok/s ok for short replies, poor for long. |
| **Coding / API (W3)** | latency-tolerant | Offload **viable** at ~7 tok/s (8 GB cap) — a 26–35B brain on a modest machine. |
| **Background / batch (W4)** | throughput only | Offload **clearly useful** — run oversized models on small-RAM hosts. |

### The Jetson Orin Nano (8 GB) question — honest, untested answer

My cgroup test does **not** model a Jetson, for two reasons: (a) Intel Vulkan pins GPU memory in
physical RAM and can't page it from NVMe; (b) Jetson has **true unified memory** (CUDA managed
memory with GPU-side page faulting / oversubscription), an architecture Intel+Vulkan lacks. So on
a Jetson the GPU *might* compute on oversubscribed, NVMe-backed memory — a path unavailable here.
Realistic expectation for a 14 GB model on an 8 GB Jetson: single-digit tok/s either way
(oversubscribed ~2×, NVMe-bound), fine for W3/W4, not for voice. **Needs a Jetson to confirm; I
cannot validate it on this hardware.** Note also Jetson NVMe is typically slower than 7.6 GB/s,
which would push the numbers down from my results.

### Partial-offload knee + energy cost (NEW, 2026-07-06; Gemma 26B-A4B, EN, RAPL package power)

| `--n-cpu-moe` | tok/s | J/token | avg W | note |
|---|---|---|---|---|
| 0 (all experts on iGPU) | 17.3 | **1.08** | 18.7 | baseline |
| 4 | 11.2 | 1.52 | 17.0 | −35% for just 4 layers |
| 8 | 10.0 | 1.78 | 17.8 | |
| 16 | 9.4 | 1.95 | 18.4 | |
| 32 | 8.7 | 2.11 | 18.3 | |
| 99 (all experts on CPU) | 7.2 | **2.43** | 17.5 | |

Two findings that settle the "do we lose anything by offloading?" question:

1. **There is no gentle knee.** Offloading even *4* expert layers drops decode 17.3 → 11.2 tok/s
   (−35%). The first offloaded layers cost the most (each forces a per-token GPU↔CPU sync);
   layers 32→99 are nearly free by comparison. So "the model almost fits, just spill a few
   layers" is a *bad* deal — you pay most of the penalty for the first spill. Fitting entirely
   on the GPU (via quant) is dramatically better than spilling a little.
2. **Energy per token ~2.25× worse on CPU** (1.08 → 2.43 J/token). Average wattage is flat
   (~17–19 W, same SoC package) — the extra energy is purely from taking longer. **The iGPU is
   ~2.4× more energy-efficient per token.** On a laptop on battery this is a first-class cost,
   not a footnote. Plus CPU offload consumes the very cores Fono reserves for the realtime audio
   pipeline (STT/TTS/VAD) — a third, qualitative penalty for the voice use case.

**Answer to "do we lose anything offloading even with enough RAM?": yes — ~2.4× speed, ~2.25×
energy/token, and CPU-core contention with the audio pipeline. The only upside is freeing
physical RAM. So never offload-when-it-fits for voice; offload is strictly a
can't-fit-on-GPU fallback.**

### Driver/hardware reality for "iGPU + NVMe paging" (NEW, 2026-07-06)

Measured on the host: kernel **7.0.0+**, **`xe`** DRM driver (not legacy i915), **Arc 140V
(Lunar Lake Xe2)** GPU, **Level-Zero loader installed** (`libze_loader.so.1.27.0`), no oneAPI
compiler (`icpx` absent). Conclusions:

- **Not a kernel problem — the host is already on the newest stack.** Landing a newer kernel
  unlocks nothing here; `xe` + Xe2 already expose the SVM/recoverable-fault *hardware*.
- **It's the Vulkan memory model + llama.cpp's Vulkan backend.** Vulkan device memory is not
  demand-paged from files; llama.cpp allocates pinned device buffers. No kernel/driver upgrade
  changes that within the Vulkan path.
- **The only plausible "GPU computes on oversubscribed memory" route on Intel is Level-Zero USM
  via llama.cpp's SYCL backend** — which requires building llama.cpp with the oneAPI/DPC++
  compiler (not installed) *and* USM oversubscription actually being fast (unproven). That's a
  research experiment, not a config switch.
- **Practical takeaway:** don't chase kernels or NVMe-GPU paging. Either make the model *fit* the
  GPU (quantization — below), or accept the slow CPU path for non-voice workloads.

### The ds4 lever that DOES work on this hardware: shrink experts to *fit the GPU*

The genuinely useful ds4 idea for Intel/AMD iGPU laptops is **not** NVMe streaming — it's
**asymmetric quantization** (2-bit imatrix routed experts, high-precision attention/shared/
router). If it shrinks the 26B-A4B from 14 GB to ~8–9 GB, the model **fits the GPU budget on a
16 GB machine** and runs at full ~20 tok/s. That converts an offload-only model into a
GPU-resident voice-tier model. This is the offload-family lever worth chasing for W1 — measure a
2-bit-expert GGUF's quality + size next.

### Rejected / conditional summary

- Speculative decoding (all forms): rejected with measurements (EAGLE-3 −25% Romanian; MTP is
  server-layer; n-gram net loss). Revisit only on upstream C-API exposure.
- Expert offload beyond the GPU budget: **conditional** (was "parked" in v8). Useless for voice;
  viable for W3/W4 and small-RAM hardware at 4–8 tok/s. Gated by workload + hardware, not on by
  default.

## Implementation Plan

### Phase A — prerequisite checks — **COMPLETE**

- [x] Task A1. Pinned `llama-cpp-2` architecture support for Gemma 4 MoE and Qwen3.6 verified.
- [x] Task A2. Core C API reachability of `tensor_buft_overrides` / `use_mmap` confirmed
      (`llama.h:299,323`). Relevant again for Phase D (now conditional, not parked).

### Phase B — model introspection + tiers (the headline feature)

- [ ] Task B1. GGUF introspection in the embedded backend: read `{arch}.expert_count`,
      `expert_used_count`, layer/head/dim counts, file size + quant type, and
      `tokenizer.chat_template` — at selection/load. Single source of truth; works for
      user-supplied GGUFs too; zero per-model maintenance.
- [ ] Task B2. Derive technique application from introspection: thinking-marker template ⇒
      prevention + scrub (Phase C); KV dims + ctx ⇒ memory budget check; MoE + total-size vs
      **GPU budget** ⇒ GPU-resident tier eligibility (below) vs offload tier eligibility.
      Missing metadata ⇒ conservative defaults, never a hard failure.
- [ ] Task B3. Registry stays curated-distribution-only: add the two MoE rows (Gemma 4 26B-A4B
      QAT Q4_0; Qwen3.6-35B-A3B UD-Q3_K_XL), pinned SHA256 + Apache-2.0. No technique metadata.
- [ ] Task B4. **Tiering keyed on GPU budget (the decisive gate):**
      - *Fast/voice tier*: offer an MoE (or dense) model only when its full weights fit the
        host's usable GPU/UMA budget → the measured ~20 tok/s regime.
      - *Do not* advertise an oversized MoE as a fast tier (off-GPU experts = dense-12B speed).
      - E2B stays the universal floor. Wizard nudges iGPU hosts to the gpu binary.
      - Budget estimate uses the Vulkan device heap report (measured: `23679 MiB` on this iGPU)
        with conservative headroom; a first-run decode self-check can demote a mis-estimated tier.
- [ ] Task B5. ADR recording: MoE-tier decision, introspection-over-metadata, the UMA/cgroup
      footprint finding, the workload-gated offload verdict, and the measured evidence.

### Phase C — correctness hardening (required by any modern model)

- [ ] Task C1. Template-driven thinking prevention from the *introspected* chat template
      (`crates/fono-polish/src/llama_local.rs:898-996`), not model-name matching (Qwen
      `<think>` seeding; Gemma 4 channel format).
- [ ] Task C2. Output-side scrub for known thinking-marker families — defense in depth; leaked
      markers would be spoken by TTS today.
- [ ] Task C3. KV-cache Q8_0 + flash-attention in embedded context params where the bindings
      expose them, gated by introspected KV dims + ctx. Long-context enabler (no speed effect at
      ctx ≤4096; ~47% KV saving at ctx ≥8k / multi-slot). Verified working on Vulkan.

### Phase D — SSD-streaming mode for oversized MoE models (**the product requirement, reframed**)

Goal restated (2026-07-06 review): when a model does not fit — either the machine is small OR
other apps (browser, IDE) are using the RAM — Fono runs it in **SSD streaming mode**: non-routed
weights stay resident (GPU/UMA), routed experts live in an in-memory cache and are loaded from
the GGUF on cache miss. Streaming is slower than fitting, but usable, because routed experts
dominate model size and modern NVMe absorbs the misses.

**Key realisation: this mode already exists in llama.cpp — it is exactly what R3/R4 measured.**
`-ngl 99 --n-cpu-moe 99` + mmap (default) IS the ds4 architecture: attention/shared/router
weights resident on the iGPU, routed experts mmap'd from the GGUF, the **kernel page cache as
the in-memory expert cache** (LRU, dynamically sized, shrinks when Firefox needs RAM and regrows
after — no cap needed in production; the cgroup in the benchmark only simulated that pressure).
llama.cpp even does the right fadvise/madvise calls already (`llama-mmap.cpp:451-470`). What
Fono needs is **glue** (detect + configure), plus optional upstream patches to close the speed
gap. Measured today: ~5–8 tok/s with a healthy cache, ~3–5 under severe pressure, vs ~20
GPU-resident.

- [x] Task D1. Full offload matrix measured (R1/GPU-4G/R2/R3/R4, both models; knee sweep;
      energy) — see tables.
- [ ] Task D2. **Ship streaming mode as glue (universal, no patch):**
      - `tensor_buft_overrides` passthrough in the embedded backend (= `--n-cpu-moe`); confirm
        `llama-cpp-2` surface, else a small params patch.
      - Auto-engage via introspection when weights exceed the *current* GPU budget; keep mmap on;
        disable model warmup in this mode (warmup touches every expert ⇒ reads the whole file at
        startup; `common.h:579`).
      - Never engage when the model fits (measured: −2.4× speed, −2.25× energy, audio-core
        contention). Voice tier keeps requiring GPU-resident.
      - Optional "leave N GB for other apps" knob: Fono sets `memory.high` on its own scope
        (Linux; no-op elsewhere) — the productised version of the benchmark's cgroup cap.
- [ ] Task D3. **Measure a 2-bit-expert (asymmetric quant) GGUF** of the 26B-A4B: if it drops to
      ~8–9 GB it fits the GPU on 16 GB machines at full speed — shrinking beats streaming
      whenever quality holds; streaming remains for what still doesn't fit.
- [ ] Task D4 (upstream patch candidates, in order of universality/effort — the "fresh ideas"):
      1. **Selective mlock** (small, universal): `--mlock` is all-or-nothing today. Patch to pin
         *only* non-routed weights + KV cache, so external memory pressure evicts expert cache
         pages (recoverable, by design) instead of latency-critical weights. Protects TTFT and
         prefill under exactly the Firefox scenario.
      2. **Temporal expert prefetch** (small-medium, universal): consecutive tokens reuse experts
         heavily (temporal locality is why R3 ≈ R2). Prefetch previous-token expert pages via
         `madvise(WILLNEED)` before the FFN needs them — hides NVMe fault latency inside compute.
      3. **UMA GPU-borrowed expert compute** (medium-large, the real prize): the Vulkan backend
         already prefers host-visible memory on UMA for "direct tensor borrowing"
         (`ggml-vulkan.cpp:3258`) and supports `VK_EXT_external_memory_host` (`:5848`). Patch =
         schedule expert matmuls on the iGPU with weights *borrowed from the mmap'd file pages*
         instead of copied into pinned device buffers. Would restore GPU-class expert compute
         (~15–20 tok/s) while keeping the evictable file-backed cache. Works on any UMA iGPU
         (Intel + AMD APUs), Vulkan-level ⇒ vendor-agnostic. Upstream-worthy; needs a
         feasibility spike (page-pinning semantics of host-pointer import during faults).
      4. **Xe SVM / SYCL USM** (watch, do not build): the kernel Xe SVM RFC would eventually let
         the GPU page-fault on system memory, but it is not mainline, only reachable via the
         SYCL backend (needs oneAPI toolchain), Intel-only, and USM shared allocations are not
         file-backed anyway — it solves oversubscription, not GGUF streaming. Re-evaluate when
         mainlined + exposed.

## Explicitly out of scope (tracked elsewhere)

- Grammar-constrained tool calls + tool-call prompt tuning → future tool-use plan.
- Concurrent TTS / ORT session sharing → separate audio-stack design task.
- Speculative decoding in any form → rejected with measurements; revisit on upstream C-API.
- Managed `llama-server` sidecar → rejected: single-binary/one-maintainer constraint.

## Verification Criteria

- `fono` (cpu variant) stays within the 25 MiB size budget; size-budget gate green.
- On a 16 GB+ iGPU machine (gpu binary), the wizard offers a GPU-fitting MoE tier reaching
  ≥15 tok/s decode with ≤1.2 s TTFT at ctx 2048.
- An MoE that does *not* fit the GPU budget is not offered as a fast tier; a fitting dense model
  is preferred for voice, while the opt-in low-RAM/large-model mode may still expose it for
  latency-tolerant workloads.
- On a 4–8 GB machine, voice behaviour unchanged (E2B floor intact); large-model mode available
  opt-in only.
- A registry-absent GGUF via manual config gets correct treatment purely from introspection.
- No thinking markers reach TTS or injected text (unit tests over scrub + template selection).
- Adding a curated model touches only the registry download table.

## Potential Risks and Mitigations

1. **GGUF metadata missing/nonstandard.** Degrade to conservative defaults; never hard-fail.
2. **Chat-template sniffing misclassifies a thinking format.** C2 output scrub is the net.
3. **Bindings lack a pre-load header-parse API.** Load-with-mmap-then-inspect, or a ~200-line
   pure-Rust GGUF header reader (no new dependency).
4. **GPU-budget estimation is vendor/driver-dependent** (only Intel Lunar Lake UMA measured).
   This gate is the linchpin. Conservative headroom + Vulkan probe + first-run decode self-check
   that demotes a mis-estimated tier.
5. **Large-model NVMe mode wears the SSD and floods I/O** (130–360 MB/token). Opt-in, clearly
   labelled, latency-tolerant workloads only; never the default.
6. **14–17 GB downloads + long GPU cold-load (~11–21 s).** Wizard states size/disk/first-load
   time up front; floor tier default when headroom marginal; downloader resumes + verifies SHA.

## Alternative Approaches

1. Static registry technique-metadata (v6) — silently mis-serves unlisted models; superseded.
2. Managed `llama-server` child — rejected on single-binary/maintenance grounds.
3. Vendored speculation — rejected on win/cost ratio.
4. NVMe expert streaming as a *voice* solution — rejected: ~4–8 tok/s, below the voice floor,
   and (v9) doesn't even save physical RAM when compared to the GPU path's actual RAM use. Kept
   only as an opt-in latency-tolerant / small-RAM capability (Phase D2).
