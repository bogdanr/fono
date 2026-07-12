# Local LLM Upgrade — Embedded-Only Shipping Plan

## Objective

Ship the measured headline win of the 2026-07 technique exploration — MoE-class local models
that are **both smarter and 2–3× faster** than the dense alternative on laptop-class hardware —
through Fono's **existing embedded `llama-cpp-2` path**, with ~zero binary growth, no sidecar
artifacts, and full resilience to model churn (a future model = one registry row).

This supersedes the open questions in
`plans/2026-07-05-local-llm-technique-exploration-v5.md`. Design constraints ratified during
review: single binary (cpu + gpu variants, both already shipped by CI/release); one-maintainer
project — no additional downloadable runtime artifacts; every technique must justify its
size/maintenance cost via the intelligence×speed trade-off; model selection must adapt to the
host machine (registry + hwcheck tiers) because both hardware and the "best model" change
constantly.

## Measured evidence base (from v4/v5, Intel Lunar Lake iGPU, Vulkan)

| Model | Shape | EN/RO/CODE tok/s | TTFT | Verdict |
|---|---|---|---|---|
| Gemma 4 E2B (current default) | dense 2B-class | fast but weakest quality (RO factual 62.5%) | ~0.3 s | stays as floor tier |
| Gemma 4 12B | dense | 12.0 / 8.9 / 8.9 | 0.6–0.8 s | not competitive — dominated by MoE |
| Gemma 4 26B-A4B Q4_0 (14.4 GB) | MoE ~4B active | 24.3 / 18.7 / 20.3 | 0.7–1.1 s | new high tier candidate |
| Qwen3.6-35B-A3B Q3_K_XL (17.2 GB) | MoE ~3B active | 21.3 / 17.6 / 18.5 | 0.9–1.8 s | new high tier candidate |

Techniques measured and **rejected** (win does not justify embedding cost): draft-model /
EAGLE-3 / MTP speculative decoding (server-layer code, workload- and language-sensitive —
EAGLE-3 regressed Romanian by 25% at 7% acceptance), n-gram self-speculation (net loss on
fresh generative output). Revisit only if upstream exposes speculation through the core C API.

Techniques accepted (embedded-compatible, ~zero cost): MoE model class itself, KV-Q8 +
flash-attention (long-context enabler, not a today-win), mmap + expert offload via
`tensor_buft_overrides` (core C API, `llama.h:299` — pending win measurement), thinking-format
handling.

## Implementation Plan

### Phase A — prerequisite checks (small, do first)

- [ ] Task A1. Verify the pinned `llama-cpp-2` version supports the target architectures:
      Gemma 4 MoE (26B-A4B family) and Qwen3.6's hybrid gated-DeltaNet MoE (`qwen35moe` arch,
      `GATED_DELTA_NET` op). If too old, bump the dependency and run the full pre-commit +
      size-budget gates; a version bump is the expected worst case, not new code.
      Rationale: this is the single gate between the measured win and the embedded path.
- [ ] Task A2. Check whether `llama-cpp-2` exposes `llama_model_params.tensor_buft_overrides`
      and the `use_mmap` toggle. Record findings; if absent, scope the minimal bindings
      addition (a params field passthrough, not vendored server code).
      Rationale: decides the cost side of the expert-offload (ds4) line.

### Phase B — the headline feature (MoE models per hardware tier)

- [ ] Task B1. Extend the model registry descriptor (`crates/fono-polish/src/registry.rs`)
      with technique-capability metadata: model shape (dense/MoE), active-parameter class,
      approximate resident working set vs file size, context-length ceiling, and
      thinking-format family (none / qwen-think / gemma-channel). Keep it a static table —
      one row per model, no code per model.
      Rationale: this is the model-churn insurance — future models are data, not code.
- [ ] Task B2. Add registry entries for the two measured MoE candidates (Gemma 4 26B-A4B QAT
      Q4_0; Qwen3.6-35B-A3B UD-Q3_K_XL) with pinned SHA256s and Apache-2.0 licence fields
      (both verified clean against ADR 0004 in Phase 1 desk checks).
- [ ] Task B3. Wire tier selection: extend `LocalTier` mapping (`crates/fono-core/src/hwcheck.rs`)
      and the wizard so ≥16 GB machines (and the gpu binary + working Vulkan probe) are offered
      the MoE tier, with E2B remaining the universal floor and the existing tiers between.
      Include the wizard nudge toward the gpu binary variant on iGPU-capable hosts.
      Rationale: "best model for the computer where it's running," using plumbing that exists.
- [ ] Task B4. Update ADR 0004 (or add a new ADR) recording the MoE-tier default-eligibility
      decision and the measured evidence, per the docs/decisions convention.

### Phase C — correctness hardening (required by any modern model)

- [ ] Task C1. Extend the embedded prompt-template builder
      (`crates/fono-polish/src/llama_local.rs:898-996`) to cover Gemma 4's thinking-channel
      format the same way Qwen's `<think>` seeding is handled today (request-side prevention).
- [ ] Task C2. Add an output-side scrub for known thinking markers
      (`<think>…</think>`, `<|channel>thought…` families) in the polish/assistant response
      path as defense-in-depth — the benchmark proved prevention alone leaks on templates it
      doesn't know, and leaked markers would be spoken aloud by TTS.
- [ ] Task C3. Enable KV-cache Q8_0 + flash-attention in the embedded backend context params
      where the bindings expose them, gated per-model via the registry descriptor. Label in
      code comments as a long-context enabler (measured: no speed win, no speed cost at
      ctx ≤4096; ~47% KV saving matters only at ctx ≥8k or multi-slot).

### Phase D — the ds4 line (expert offload; measure, then decide)

- [ ] Task D1. Run the one missing benchmark: Qwen3.6-35B-A3B (17 GB file) with experts forced
      to CPU/mmap (`--n-cpu-moe` sweep via the existing `../fono-tmp` harness) to simulate the
      "model bigger than available RAM" scenario, recording tok/s and TTFT. Execution task —
      needs a shell-capable agent or manual run; harness and models are already in place.
- [ ] Task D2. Decision gate: if constrained decode stays ≥8 tok/s, promote expert offload to
      implementation — add the `tensor_buft_overrides` passthrough (per Task A2 findings),
      an "oversized MoE" registry flag with resident-set gating in hwcheck, and NVMe-speed
      awareness. If it falls below, park the ds4 line with the numbers recorded, alongside the
      speculation rejection.

## Explicitly out of scope (tracked elsewhere)

- Grammar-constrained tool calls and tool-call prompt tuning → future tool-use plan.
- Concurrent TTS / ORT session sharing → separate audio-stack item; contention with decode
  threads makes it a design task, not a quick win.
- Speculative decoding in any form → rejected with measurements; revisit on upstream C-API
  exposure only.
- Managed `llama-server` sidecar → rejected: violates single-binary/one-maintainer constraint.

## Verification Criteria

- `fono` binary (cpu variant) stays within the 25 MiB size budget after all phases;
  size-budget gate green.
- On a 16 GB+ iGPU machine with the gpu binary, the wizard offers an MoE-tier model and the
  assistant reaches ≥15 tok/s decode with ≤1.2 s TTFT at ctx 2048 (matches measured numbers).
- On a 4–8 GB machine, behaviour is unchanged (E2B floor intact).
- No thinking markers reach TTS or injected text for Qwen-think and Gemma-channel families
  (unit tests over the scrub + template builders).
- A hypothetical new MoE model can be added by editing only the registry table (compile-time
  check: no other file needs touching).
- Phase D decision recorded with measured tok/s either way.

## Potential Risks and Mitigations

1. **Pinned `llama-cpp-2` too old for the new architectures.**
   Mitigation: Task A1 first; a bindings bump is contained and testable by the existing gates.
2. **Bindings don't expose needed params (tensor overrides, KV type, flash-attn).**
   Mitigation: Task A2 scopes it; upstream `llama-cpp-2` PR or a minimal patch — params
   passthrough only, never vendored server logic.
3. **Vulkan behaviour differs across iGPU vendors (only Intel Lunar Lake measured).**
   Mitigation: tier gating keys on the Vulkan probe succeeding, with CPU fallback; treat other
   iGPUs as unvalidated until user reports or a second test machine.
4. **14–17 GB model downloads on first run.**
   Mitigation: wizard states size + disk requirement up front; floor tiers remain default when
   headroom is marginal; existing downloader already does resume + SHA verification.
5. **Model churn invalidating measured candidates before shipping.**
   Mitigation: the registry-row design makes swapping candidates cheap by construction; the
   benchmark harness in `../fono-tmp` is reusable for any new GGUF in under an hour.

## Alternative Approaches

1. Managed `llama-server` child process — richer techniques for free, but a second artifact
   across all platforms; rejected on maintenance/single-binary grounds.
2. Ship speculative decoding embedded by vendoring `common/speculative` — +30–35% on code
   workloads only; permanent fork-maintenance cost; rejected on win/cost ratio.
3. Wait for upstream C-API speculation support — passive path; revisit periodically at
   llama.cpp bumps.
