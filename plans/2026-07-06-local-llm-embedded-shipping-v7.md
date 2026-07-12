# Local LLM Upgrade — Embedded-Only Shipping Plan

## Objective

Ship the measured headline win of the 2026-07 technique exploration — MoE-class local models
that are **both smarter and 2–3× faster** than the dense alternative on laptop-class hardware —
through Fono's **existing embedded `llama-cpp-2` path**, with ~zero binary growth, no sidecar
artifacts, and full resilience to model churn.

v7 changes vs v6: Phase A marked complete (verified during the exploration); Phase B redesigned
around **GGUF self-description instead of registry metadata** — technique decisions are derived
from the model file itself at selection/load time, so *any* model (including user-supplied ones
not in the registry) gets correct treatment automatically. The registry returns to its original
narrow job: curated download distribution (URL, SHA256, licence, display name).

Design constraints (ratified): single binary (cpu + gpu variants already shipped by CI/release);
one-maintainer project — no additional downloadable runtime artifacts; every technique must
justify its cost via the intelligence×speed trade-off; model selection adapts to the host
machine; models change every couple of months — the design must not require code per model.

## Measured evidence base (from v4/v5, Intel Lunar Lake iGPU, Vulkan)

| Model | Shape | EN/RO/CODE tok/s | TTFT | Verdict |
|---|---|---|---|---|
| Gemma 4 E2B (current default) | dense 2B-class | fast, weakest quality (RO factual 62.5%) | ~0.3 s | stays as floor tier |
| Gemma 4 12B | dense | 12.0 / 8.9 / 8.9 | 0.6–0.8 s | dominated by MoE |
| Gemma 4 26B-A4B Q4_0 (14.4 GB) | MoE ~4B active | 24.3 / 18.7 / 20.3 | 0.7–1.1 s | high-tier candidate |
| Qwen3.6-35B-A3B Q3_K_XL (17.2 GB) | MoE ~3B active | 21.3 / 17.6 / 18.5 | 0.9–1.8 s | high-tier candidate |

Rejected with measurements (win doesn't justify embedding cost): all speculative decoding
(EAGLE-3 regressed Romanian −25% at 7% acceptance; MTP is server-layer; n-gram is a net loss on
fresh output). Revisit only if upstream exposes speculation via the core C API.

## Implementation Plan

### Phase A — prerequisite checks — **COMPLETE**

- [x] Task A1. Pinned `llama-cpp-2` architecture support for Gemma 4 MoE and Qwen3.6
      (`qwen35moe`, `GATED_DELTA_NET`) — verified during the exploration.
- [x] Task A2. Core C API reachability of `tensor_buft_overrides` / `use_mmap` confirmed
      (`llama.h:299,323`); exact `llama-cpp-2` surface to be confirmed opportunistically in
      Phase D implementation (worst case: minimal params passthrough patch).

### Phase B — model introspection + MoE tiers (the headline feature)

- [ ] Task B1. Build a GGUF introspection step in the embedded backend: on model
      selection/load, read from the file header — `{arch}.expert_count` and
      `expert_used_count` (dense vs MoE, active-parameter class), layer/head/dim counts
      (KV-cache cost prediction), file size + quant type (working-set estimate), and
      `tokenizer.chat_template` (thinking-format detection). Prefer the bindings' existing
      metadata APIs post-load; add a lightweight pre-load header parse only if tier gating
      needs the answer before committing to a load.
      Rationale: the model file is the single source of truth — works for registry models and
      arbitrary user-supplied GGUFs alike; zero per-model maintenance.
- [ ] Task B2. Derive technique application from introspection, not from lists: MoE ⇒ expert
      offload eligible (Phase D); template contains thinking markers ⇒ matching prevention +
      scrub (Phase C); KV dims + requested ctx ⇒ memory budget check. Unknown/missing metadata
      degrades to today's conservative defaults — never a hard failure.
- [ ] Task B3. Keep the registry (`crates/fono-polish/src/registry.rs`) as curated
      distribution only: add the two MoE candidate rows (Gemma 4 26B-A4B QAT Q4_0;
      Qwen3.6-35B-A3B UD-Q3_K_XL) with pinned SHA256 + Apache-2.0 licence (both verified
      against ADR 0004). No shape/technique metadata in the table.
- [ ] Task B4. Tier selection: extend `LocalTier` mapping (`crates/fono-core/src/hwcheck.rs`)
      and the wizard so ≥16 GB machines with the gpu binary + passing Vulkan probe are offered
      the MoE tier (using introspected working-set vs available RAM as the runnability check,
      replacing static `approx_mb`-style assumptions). E2B stays the universal floor. Wizard
      nudges iGPU-capable hosts toward the gpu binary variant.
- [ ] Task B5. ADR recording the MoE-tier decision, the introspection-over-metadata design,
      and the measured evidence, per docs/decisions convention.

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

### Phase D — the ds4 line (expert offload; measure, then decide)

- [ ] Task D1. Run the one missing benchmark: Qwen3.6-35B-A3B (17 GB) with experts forced to
      CPU/mmap (`--n-cpu-moe` sweep via the existing `../fono-tmp` harness) to simulate
      "model bigger than available RAM"; record tok/s + TTFT. Execution task (shell-capable
      agent or manual); harness and models already in place.
- [ ] Task D2. Decision gate: constrained decode ≥8 tok/s ⇒ implement — `tensor_buft_overrides`
      passthrough in the embedded backend, introspection-driven "oversized MoE" runnability
      logic in hwcheck (resident-set vs RAM, NVMe-speed awareness). Below 8 tok/s ⇒ park the
      ds4 line with numbers recorded, alongside the speculation rejection.

## Explicitly out of scope (tracked elsewhere)

- Grammar-constrained tool calls + tool-call prompt tuning → future tool-use plan.
- Concurrent TTS / ORT session sharing → separate audio-stack design task (decode-thread
  contention makes it non-trivial).
- Speculative decoding in any form → rejected with measurements; revisit on upstream C-API
  exposure.
- Managed `llama-server` sidecar → rejected: violates single-binary/one-maintainer constraint.

## Verification Criteria

- `fono` (cpu variant) stays within the 25 MiB size budget; size-budget gate green.
- On a 16 GB+ iGPU machine with the gpu binary, the wizard offers an MoE-tier model and the
  assistant reaches ≥15 tok/s decode with ≤1.2 s TTFT at ctx 2048.
- On a 4–8 GB machine, behaviour unchanged (E2B floor intact).
- **A GGUF absent from the registry, supplied via manual config, gets correct technique
  treatment (MoE detection, thinking handling, memory gating) purely from introspection** —
  covered by a unit/integration test with a crafted header.
- No thinking markers reach TTS or injected text for Qwen-think and Gemma-channel families
  (unit tests over scrub + template selection).
- Adding a curated model touches only the registry download table.
- Phase D decision recorded with measured tok/s either way.

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
4. **Vulkan behaviour differs across iGPU vendors (only Intel Lunar Lake measured).**
   Mitigation: tier gating keys on the Vulkan probe; CPU fallback; other iGPUs unvalidated
   until tested.
5. **14–17 GB first-run downloads.**
   Mitigation: wizard states size/disk up front; floor tier remains default when headroom is
   marginal; downloader already resumes + verifies SHA.

## Alternative Approaches

1. Static registry technique-metadata (v6 design) — simpler to implement but silently
   mis-serves unlisted models and adds per-model maintenance; superseded by introspection.
2. Managed `llama-server` child process — rejected on single-binary/maintenance grounds.
3. Vendored speculation — rejected on win/cost ratio (+30% code-only vs permanent fork).
