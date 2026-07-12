# Local LLM Technique Exploration — Best Model per Workload on Consumer Hardware

## Objective

Determine, with measurements on the reference laptop (Intel Lunar Lake iGPU, Vulkan/UMA, 30 GB RAM),
which combination of inference techniques lets Fono serve the *smartest viable model per workload*
through its assistant and its OpenAI/Ollama-compatible API — treating Gemma 4 4B-class as the floor,
not the target. Output: a per-workload recommendation (model + technique stack + resource envelope)
backed by data, ready to become an ADR + tier/registry changes.

## Workload Definitions and Success Criteria

Every technique is judged **per workload**, because latency budgets differ by two orders of magnitude:

| ID | Workload | Latency requirement | Quality requirement | Shape |
|----|----------|--------------------|--------------------|-------|
| W1 | Voice assistant (Fono built-in) | TTFT < 1 s, decode ≥ 10 tok/s (TTS streaming pace) | RO+EN factual/tool fixtures | Short prompts, short replies, 2-turn tool calls |
| W2 | Smart-home / agentic tool-use | TTFT < 2 s, total turn < 5 s | Tool-name/args correctness ≥ 0.9 | Medium prompts (tool schemas), JSON outputs |
| W3 | Programming via API | TTFT tolerant (< 10 s), decode ≥ 15 tok/s sustained | Code-edit/gen pass rate | Long prompts (4–32 k ctx), long outputs, output often echoes input |
| W4 | Background API tasks (summarise, translate, classify) | Throughput-bound; latency ~irrelevant | Task pass rate | Batchable, parallel slots |

Baselines already measured: Gemma 4 E2B (RO factual 62.5 %, tool mean 0.72, p50 0.3 s) and
Gemma 4 12B dense Q4_0 Vulkan (RO factual 100 %, tool mean 0.90+, 8.6 tok/s decode, TTFT ~0.7 s,
pp 178 tok/s) — the "floor" and the "naive smart" reference points.

## Technique Inventory (including ones not previously suggested)

- T1 **Speculative decoding, draft model** (`llama-server --model-draft`) — DSpark/DeepSpec-inspired; decode multiplier, language-sensitive acceptance.
- T2 **Prompt-lookup / n-gram speculative decoding** (`--spec-type ngram`-style, no draft model) — free memory-wise; strongest exactly where output copies input (W3 code edits, W2 JSON echoing schemas).
- T3 **MoE expert offload** (`--n-cpu-moe` / `-ot` tensor overrides) — ds4-inspired; big-model quality at small active-param decode cost.
- T4 **Asymmetric quantization** (imatrix 2–3-bit routed experts, high-precision attention/shared/router) — shrinks MoE footprint to laptop RAM.
- T5 **KV-cache quantization** (`-ctk/-ctv q8_0`) — buys context length or an extra parallel slot from the same memory.
- T6 **Grammar-constrained decoding** (GBNF / JSON schema on tool calls) — guarantees valid tool JSON; likely fixes the observed verbosity/leak failures at zero cost.
- T7 **Reasoning-budget control** (thinking on/off/limited per workload) — thinking off for W1/W2, bounded for W3.
- T8 **Prefix/prompt caching + cache-reuse across requests** (`--cache-reuse`, per-slot caches) — Fono already has embedded prefix caching; verify server-side equivalent per workload.
- T9 **Parallel slots / continuous batching** (`-np N`) — W4 throughput; interacts with KV memory (T5).
- T10 **Model cascade / routing** — small model answers easy turns, escalates hard ones to the big model (both resident via UMA or on-demand); alternative to making one model fast.
- T11 **On-demand model swap** (llama-swap-style: load per-workload model on first API request, evict on idle) — alternative to cascade when RAM cannot hold two.

## Candidate Models

- M0 Gemma 4 4B / E2B-class (floor, control)
- M1 Gemma 4 12B QAT Q4_0 (already benchmarked dense; target for T1/T2)
- M2 Gemma 4 26B-A4B MoE (primary MoE candidate; licence check on specific artifact required)
- M3 Qwen3.6 35B-A3B non-thinking (Apache-2.0 MoE alternative; check for MTP/draft head)
- M4 A current coder-tuned small/MoE model for W3 (candidate chosen at Phase B time by licence + freshness)
- D1 Small same-tokenizer Gemma draft (for T1)

## Implementation Plan

### Phase A — Fixture and harness gaps (prerequisite; no model downloads)

- [ ] Task A1. Define W3 coding fixture suite (10–15 fixtures: small code-gen, code-edit where output echoes input, RO/EN comments) in `tests/fixtures/` following the existing `assistant_factual` TOML pattern — needed because fono-bench has no coding suite and W3 is where T2 should shine.
- [ ] Task A2. Add a sustained-throughput bench mode (or a documented `llama-bench`/script equivalent) measuring decode tok/s over ≥ 512-token generations and under `-np 2/4` parallel load — existing suites measure single short turns only, blinding us to W4.
- [ ] Task A3. Add draft-acceptance-rate capture to the benchmark protocol (llama-server logs expose accepted/rejected draft tokens) — the single number that decides T1's fate per language.
- [ ] Task A4. Fix or file the thinking-channel leak: strip/parse `<|channel>thought` markers in the assistant/polish response path before any further model evaluation, so quality scores are not polluted and TTS is safe. (Implementation via Forge; blocking for honest W1 numbers.)

### Phase B — Licence and artifact vetting (desk work, parallel with A)

- [ ] Task B1. Verify licences of M2/M3/M4 specific GGUF artifacts and their base models against ADR 0004 criteria; exclude any non-OSI candidates (e.g. EXAONE) before spending compute on them.
- [ ] Task B2. Identify the best available D1 draft model and any DeepSpec-trained draft checkpoints for Gemma/Qwen families; record licences and sizes.
- [ ] Task B3. Confirm current llama.cpp Vulkan support status for: MoE expert offload on UMA iGPUs, KV quantization, n-gram speculative, flash attention — a technique unsupported on Vulkan is disqualified for this hardware regardless of paper numbers.

### Phase C — Single-technique measurements (each vs the M1-dense baseline)

Decision-gated: each run has a kill criterion so we stop early instead of completing the matrix for its own sake.

- [ ] Task C1. T1 on M1 (12B + draft): measure decode multiplier and acceptance rate on EN and RO separately, W1+W2 fixtures. **Gate:** acceptance < 60 % on RO ⇒ drop T1 for voice, keep for W3/W4 only.
- [ ] Task C2. T2 (prompt-lookup) on M1 with W3 coding fixtures (code-edit heavy). **Gate:** < 1.3× decode on edits ⇒ drop T2.
- [ ] Task C3. T3+T4 on M2 (MoE, expert-offloaded, asymmetric quant if artifact exists; plain Q4 otherwise): decode tok/s, TTFT cold vs warm (expert-page cache effects), peak RSS + iGPU allocation, W1+W2 accuracy. **Gate:** decode < 8 tok/s or RSS forces swap ⇒ demote M2 to W3/W4-only.
- [ ] Task C4. Same as C3 on M3; also probe whether its MTP/draft head works in llama.cpp for free T1-style gains.
- [ ] Task C5. T5 (KV q8_0) on the best model so far: verify quality is unchanged on fixtures and measure the freed memory (report as extra ctx or extra slots).
- [ ] Task C6. T6 (grammar-constrained tool calls) on M1: rerun tool-use suite with a JSON grammar. **Expectation:** pass-rate jump at ~zero latency cost; if confirmed, T6 becomes unconditional for W2.
- [ ] Task C7. T9 (parallel slots) throughput curve on the best W4 candidate: tok/s aggregate at np=1/2/4 within the memory envelope from C5.

### Phase D — Stacked configurations (only the winners from C)

- [ ] Task D1. Compose the best W1 stack (e.g. M2 MoE + T6 + T7-off + T8; or M1 + T1 if acceptance held) and run the full W1 suite: TTFT, tok/s, RO+EN accuracy, RSS. Compare against both baselines.
- [ ] Task D2. Compose the best W3 stack (e.g. M4 or M3 + T2 + T5 long-ctx) and run the coding suite at 8–16 k ctx.
- [ ] Task D3. Evaluate T10 cascade vs T11 swap for serving W1 and W3 simultaneously within 30 GB UMA: measure memory headroom and switch/escalation latency.
- [ ] Task D4. 30-minute soak of the winning API configuration (llama-server, mixed W2/W4 traffic) watching for RSS creep, thermal throttling on the laptop, and p95 drift.

### Phase E — Decision and productization plan

- [ ] Task E1. Write the results ADR: per-workload recommended model + technique stack + minimum hardware tier; explicitly record rejected options and why.
- [ ] Task E2. Define the product mechanism: managed llama-server child (flags per workload profile) vs embedded llama-cpp-2 (requires upstream C-API work for T1/T2) — choose per the Gemma plan's stated long-term preference and the D3 result.
- [ ] Task E3. Map results onto the wizard/registry hardware tiers (floor = 4B-class unchanged; mid/high tiers from D1/D2 winners) and plan registry entries + licence notes.
- [ ] Task E4. List upstream contributions worth making from what we learned (llama.cpp C-API speculative surface, Vulkan MoE-offload fixes, bindings PRs) with effort estimates.

## Verification Criteria

- Every technique has a measured, per-workload number against the same fixture SHAs as the existing baselines (comparable JSON reports in `target/bench-results/` schema).
- Each Phase C task ends in an explicit keep/drop decision recorded with its gate value.
- The final ADR states, for each of W1–W4: model, quant, technique flags, TTFT, tok/s, accuracy, peak memory — reproducible from the recorded commands.
- No recommended default violates ADR 0004 licensing or the size-budget rules (server binary distribution accounted for in E2).

## Potential Risks and Mitigations

1. **Vulkan backend gaps** (MoE offload or KV-quant paths untested on Intel iGPU)
   Mitigation: Phase B3 desk-check first; where ambiguous, a 10-minute smoke run before committing to a full suite.
2. **Draft acceptance collapse on Romanian** making T1 useless for the flagship voice workload
   Mitigation: C1 measures EN/RO separately with an explicit gate; T2 and T3 are independent fallbacks.
3. **Live-Linux memory pressure** (30 GB RAM shared with tmpfs; swap already full)
   Mitigation: all models and builds stay in `../fono-tmp` on real disk; monitor `free` before every large run; MoE runs use mmap so page cache is reclaimable.
4. **Benchmark answers a question the product can't ship** (e.g. server-only features vs embedded runtime)
   Mitigation: E2 explicitly ties every winner to a shipping mechanism; techniques that only exist in llama-server count that cost in the decision.
5. **Model churn** (better models appearing mid-exploration)
   Mitigation: the deliverable is the *technique stack + harness*, model-agnostic by design; swapping a new model in is a registry entry plus one Phase D rerun.

## Alternative Approaches

1. **Skip Phase C, jump to stacked "best guess" configs (D1/D2 directly):** faster to an answer, but a bad number can't be attributed to any one technique — debugging costs more than the skipped runs saved.
2. **Cloud-first strategy (API serving deferred to cloud providers, local stays 4B-floor):** zero exploration cost, but abandons the differentiator (private local inference for programming/smart-home) that motivated this work.
3. **Adopt an external serving stack (Ollama/llama-swap) instead of Fono-managed llama-server:** less code to own, but breaks single-binary philosophy and adds a runtime dependency Fono can't pin.
