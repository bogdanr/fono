# Local LLM Technique Exploration v3 — Phase 1 Desk-Check Results

## Objective (unchanged from v2)

Serve the smartest viable local model on consumer laptop hardware (reference: Intel Lunar Lake
iGPU, Vulkan/UMA, 30 GB RAM) for all Fono workloads — voice assistant, smart home, programming,
background API tasks — via a single universal technique stack on a Fono-managed `llama-server`.

## Phase 1 Desk-Check Results (completed)

### Task 1.1 — Licence vetting (against ADR 0004)

| Artifact | Licence | Verdict |
|---|---|---|
| `google/gemma-4-26B-A4B-it-qat` (base + QAT GGUF) | Apache-2.0 | Clean — matches the E2B/E4B precedent already in ADR 0004 |
| `google/gemma-4-31B-it-qat` (base + QAT GGUF) | Apache-2.0 | Clean |
| `RedHatAI/gemma-4-26B-A4B-it-speculator.eagle3` | Apache-2.0 | Clean |
| `RedHatAI/gemma-4-31B-it-speculator.eagle3` | Apache-2.0 | Clean |
| `Qwen/Qwen3.6-35B-A3B` (new candidate, see below) | Apache-2.0 | Clean |

No ADR-0004 blockers found for any candidate. All five artifacts are default-eligible on licence
grounds alone; Phase 2 quality/performance results are the remaining gate.

### New candidate surfaced during vetting: Qwen3.6-35B-A3B

Fetched the model card directly (not from the earlier chart reading). This changes the MoE shortlist:

- **Apache-2.0**, 35B total / **3B active** params (8 routed + 1 shared expert of 256, per token) —
  same active-parameter class as Gemma 4 26B-A4B.
- **Built-in multi-token-prediction head trained natively** ("MTP: trained with multi-steps"),
  vendor-recommended speculative configs already published (vLLM: `qwen3_next_mtp`, 2 speculative
  tokens; SGLang: NEXTN, 3 steps). This is a *free* draft, no separate EAGLE-3 conversion needed,
  if llama.cpp's loader for this arch exposes the NextN layers as a llama.cpp draft path (see below).
- Benchmarks (vendor-reported, general capability, not just the earlier chart's index): beats
  Gemma 4 26B-A4B by a wide margin on coding/agentic tasks (SWE-bench Verified 73.4 vs 17.4,
  Terminal-Bench 2.0 51.5 vs 34.2) and is close to or ahead of the larger Qwen3.5-27B dense model on
  several agentic benchmarks. This directly serves the "programming via API" workload goal.
- Architecture: hybrid — 3 gated-DeltaNet (linear attention) blocks per 1 full-attention block,
  MoE FFN throughout. Not a plain transformer; verified below that llama.cpp already implements it.
- **Recommend adding this as the primary MoE candidate alongside Gemma 4 26B-A4B** in Phase 2,
  rather than treating Qwen as a fallback.

### Task 1.2 — Vulkan support verification (this exact build, `../fono-tmp/llama.cpp`)

Checked llama.cpp's own generated Vulkan op-support matrix (`docs/ops/Vulkan.csv`) and source tree,
rather than assuming from documentation:

- **`qwen35moe` architecture is natively implemented** (`src/models/qwen35moe.cpp`), including
  explicit handling of `LLM_KV_NEXTN_PREDICT_LAYERS` — "NextN/MTP (Qwen3.5/3.6): extra decoder block
  appended beyond the main stack" is a direct code comment. This means Qwen3.6-35B-A3B (same family)
  is very likely to load and run today, **and its native MTP head is already wired as a first-class
  concept in the loader**, not something we'd need to bolt on — the biggest single find of Phase 1.
- **`GATED_DELTA_NET` op has Vulkan kernels** (`ggml-vulkan/vulkan-shaders-gen.cpp`, confirmed
  "supported=1" for the head/size configurations matching this architecture in `docs/ops/Vulkan.csv`)
  — also implemented for CUDA, Metal, SYCL, OpenCL, Hexagon, and CPU, so this is a mainline-supported
  op, not an experimental one-backend addition.
- **`FLASH_ATTN_EXT` has broad Vulkan support** (`docs/ops/Vulkan.csv`) — `-ctv` KV-cache quantization
  is not blocked on this backend; no need to fall back to K-only quantization.
- **Verdict: no disqualifications found for EAGLE-3, DFlash, MoE offload, or KV quant on this
  hardware's Vulkan backend.** Actual runtime smoke-testing (does it *run correctly*, not just does
  the op exist) is still required in Phase 2 — this task de-risks the sweep, it doesn't replace it.

### Task 1.3 — Python tooling for EAGLE-3 conversion

Not yet executed — deferred into Phase 2 Task 2.1/2.2 setup, since the Qwen3.6 native-MTP finding
may make the Gemma EAGLE-3 conversion path lower priority for the MoE track specifically (dense
Gemma 4 12B/31B still needs it, since dense models have no built-in draft).

## Updated Phase 2 Candidate Matrix

| # | Model | Shape | Draft mechanism | Priority |
|---|---|---|---|---|
| 1 | Gemma 4 12B | dense | EAGLE-3 speculator (conversion needed) or ngram-mod | baseline done (8.6 tok/s); speculation run next |
| 2 | Gemma 4 26B-A4B | MoE, ~4B active | EAGLE-3 speculator (ready-made) | high |
| 3 | **Qwen3.6-35B-A3B** (new) | MoE, ~3B active | **native MTP head** (verify llama.cpp exposes it as a usable draft path) or ngram-mod fallback | **high — promote to co-primary MoE candidate** |
| 4 | Gemma 4 31B | dense | EAGLE-3 speculator (ready-made) | secondary — "patience mode" only, per earlier latency math |

## Revised Phase 2 Tasks (supersedes v2 Task 2.2 wording)

- [ ] Task 2.1. Dense track — Gemma 4 12B: add ngram-mod, KV quant, cache-reuse, and EAGLE-3 speculator runs on top of the existing baseline. Measure EN and RO separately.
- [ ] Task 2.2a. MoE track A — Gemma 4 26B-A4B: `--n-cpu-moe` sweep, ngram-mod, EAGLE-3 speculator (ready-made). Record cold vs warm TTFT.
- [ ] Task 2.2b. MoE track B (new) — Qwen3.6-35B-A3B: same sweep; additionally verify whether llama.cpp's NextN/MTP layer support in `qwen35moe.cpp` is reachable as a `--spec-type` draft path in this build (check `llama-server --help` spec-type list and `common/arg.cpp` for an MTP-specific flag) before falling back to ngram-mod. If native MTP works, this is the cheapest-to-enable speculative path of any candidate — no separate draft download/conversion at all.
- [ ] Task 2.3. Quality smoke on best dense + both MoE configs (existing fixtures via manual-endpoint escape hatch). Verify `--reasoning off` suppresses thinking-channel leakage on Qwen3.6 as well as Gemma (Qwen3.6 thinks by default per its model card — same risk class as the Gemma leak already found).
- [ ] Task 2.4. Concurrency check (`-np 2`) on whichever MoE candidate wins.

## Verification Criteria (unchanged from v2, restated)

- Every Phase 2 run recorded with exact command line, TTFT, tok/s, acceptance rate, RSS/iGPU numbers.
- Chosen default reaches ≥10 tok/s decode and ≤1 s TTFT at quality ≥ the 12B smoke baseline.
- Universal stack runs unmodified across at least one dense and two MoE models (Gemma 4 26B-A4B, Qwen3.6-35B-A3B) — stronger evidence of model-agnosticism than v2's single-MoE plan.
- No licence blocker on the shipped default (confirmed clean for all five Phase-1 candidates).
- No leaked thinking-channel markers in quality smoke output, for both Gemma-4 and Qwen3.6 candidates.

## Potential Risks and Mitigations (additions to v2)

1. **Qwen3.6's native MTP head may not be exposed as a llama.cpp speculative-decoding draft path yet** (loader support for the *architecture* is confirmed; a runtime `--spec-type` binding for it is not yet confirmed).
   Mitigation: Task 2.2b checks this explicitly before relying on it; ngram-mod is the guaranteed fallback for this model regardless.
2. **Qwen3.6's hybrid gated-DeltaNet + MoE architecture may have different UMA/offload behaviour than Gemma's plain MoE** (linear-attention state vs KV cache changes what `--n-cpu-moe` and `-ctk/-ctv` actually act on).
   Mitigation: treat Task 2.2b as its own sweep, not an assumed clone of 2.2a; record any flag that behaves differently.
3. **Op-support-matrix "supported=1" does not guarantee numerically-correct or fast Vulkan execution** — it only confirms the kernel exists and passes a shape check.
   Mitigation: Phase 2 runs are the real test; treat Task 1.2 as risk reduction, not proof.

## Alternative Approaches (unchanged from v2)

1. Embedded-runtime path with upstream C-API PRs for speculation — revisit only if the managed-server model proves operationally painful.
2. Per-workload tuned configs instead of a universal stack — rejected by design review.
3. External runtime (Ollama/llama-swap) — rejected, breaks Fono's self-contained install story.
