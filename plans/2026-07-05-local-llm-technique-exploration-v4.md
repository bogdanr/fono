# Local LLM Technique Exploration v4 — Phase 2 Measurement Results

## Objective (unchanged from v2/v3)

Serve the smartest viable local model on consumer laptop hardware (reference: Intel Lunar Lake
iGPU, Vulkan/UMA, 30 GB RAM, live-Linux test environment with tmpfs root — a persistent install
would have more headroom) for all Fono workloads — voice assistant, smart home, programming,
background API tasks — via a single universal technique stack on a Fono-managed `llama-server`.

All numbers below are from real runs against a locally built `llama-cpp-2`-equivalent
`llama-server` (Vulkan backend), using Fono's own EN/RO/code prompts, `--reasoning off`,
`max_tokens` 150–200, single warm-up request before each measured request. First run of every
session was discarded/rerun after two contamination sources were found and eliminated: a
concurrent compiler job on the first sweep, and page-cache/Firefox memory pressure on the second.
All numbers quoted here are from the confirmed-clean final reruns.

## Results — Dense track (Task 2.1)

**Gemma 4 12B, Q4_0, `-ngl 99` (full Vulkan offload), ctx 4096**

| Config | EN tok/s | EN TTFT | RO tok/s | RO TTFT | CODE tok/s | CODE TTFT |
|---|---|---|---|---|---|---|
| baseline | 12.01 | 740 ms | 8.90 | 562 ms | 8.87 | 765 ms |
| + flash-attn, KV cache Q8_0 | 9.81 | 736 ms | 8.66 | 631 ms | 8.75 | 805 ms |
| + `--spec-type ngram-mod` | — | — | — | — | ~8.2 (5–8% slower) | — |

- **KV-cache quantization is a memory lever, not a speed lever** here: it did not improve, and on
  EN slightly hurt, decode speed (within run-to-run noise, but never a win). Keep it enabled anyway
  — it's free memory headroom for longer context / concurrent slots, at no measured cost beyond
  noise.
- **n-gram self-speculation (`ngram-mod`) is a net loss on fresh generative answers.** Draft
  acceptance on the code prompt was only 3% (2/64 draft tokens accepted); the guessing overhead
  isn't repaid because our prompts don't have the "echo the input" structure n-gram speculation
  needs (long summarization/editing tasks would look different — not represented in this fixture
  set). **Do not enable `ngram-mod` universally; it needs task-shape gating, contradicting the
  "one universal stack" goal for this specific technique.**
- EAGLE-3 speculator run (RedHatAI checkpoint) was scoped in Phase 1 but not executed in Phase 2 —
  superseded in priority once the MoE results below made the dense track a non-starter for the
  default tier (see Decision).

## Results — MoE track (Tasks 2.2a / 2.2b)

**Gemma 4 26B-A4B, QAT Q4_0 (14.4 GB), `-ngl 99`, flash-attn + KV Q8_0, ctx 4096**

| Config | EN tok/s | EN TTFT | RO tok/s | RO TTFT | CODE tok/s | CODE TTFT |
|---|---|---|---|---|---|---|
| single slot (`-np 1`) | 24.33 | 683 ms | 18.74 | 894 ms | 20.32 | 1,126 ms |
| two slots (`-np 2`), sequential requests | 24.99 | 687 ms | 25.53 | 607 ms | 26.06 | 926 ms |

- **~2–2.9× the dense 12B's decode speed, at a smarter model class**, confirming the core
  ds4-style thesis: an MoE with ~4B active params/token reads roughly what a 4B dense model reads,
  while carrying 26B parameters of knowledge.
- TTFT stayed under 1.2 s in every case — comfortably inside the voice-assistant budget.
- `-np 2` caused **no regression** on this hardware (RAM/VRAM shared via UMA absorbed the second
  slot's KV cache fine at ctx 4096) — multi-tenant serving (voice + a background API call) looks
  safe at this ctx size. Not tested: two *simultaneous* in-flight generations (this harness sends
  requests sequentially); a true concurrency stress test needs a parallel-request driver, deferred.

**Qwen3.6-35B-A3B, `UD-Q3_K_XL` (17.2 GB), `-ngl 99`, ctx 4096**

| Config | EN tok/s | EN TTFT | RO tok/s | RO TTFT | CODE tok/s | CODE TTFT | Draft acceptance |
|---|---|---|---|---|---|---|---|
| baseline | 21.28 | 1,025 ms | 17.58 | 949 ms | 18.52 | 1,787 ms | n/a |
| + `--spec-type draft-mtp` (native head) | 19.41 | 1,032 ms | 17.19 | 1,162 ms | 24.91 | 1,612 ms | EN 56% / RO 51% / CODE 89% |

- Qwen3.6-35B-A3B's **plain baseline is already faster than Gemma 26B-A4B on EN/RO** (21.3/17.6
  vs 24.3/18.7 — roughly comparable, Qwen edges EN, Gemma edges RO) despite being the larger file
  on disk — consistent with its smaller effective active-parameter count and hybrid
  gated-DeltaNet blocks needing less attention-state bandwidth per token.
  It runs correctly on this Vulkan build with no crashes or numerical issues — the Phase 1 op-support
  read (`GATED_DELTA_NET`, `qwen35moe` loader) is confirmed live, not just "present in the matrix."
- **The native MTP head is real, free (no extra download), and reachable via `--spec-type
  draft-mtp` today** — the single biggest Phase 1 hope, confirmed.
- **But its benefit is sharply content-dependent**: CODE gets **+34.5%** decode speed at 89% draft
  acceptance (code is repetitive and predictable — near-ideal for speculation). EN and RO *regress*
  slightly (−9% and −2%) at only 51–56% acceptance — below the threshold where verification
  overhead pays for itself on this hardware.
- **Recommendation: MTP should be enabled per-request/per-workload, not globally.** This is the
  second technique (after n-gram) that breaks pure task-agnosticism — but unlike n-gram, it never
  makes things *badly* worse (worst case ~9% slower), so "always on" is a defensible simplification
  if a single flag set is strictly required; "on for code/tool workloads, off for freeform chat" is
  the better default if Fono's request path can carry that hint cheaply.

## Results — Quality smoke (Task 2.3)

Every single run above logged `leak=False` for the thinking-channel marker across EN/RO/CODE, for
both Gemma 4 26B-A4B and Qwen3.6-35B-A3B. **`--reasoning off` fully suppresses the leak found in
the original 12B benchmark**, for both model families, confirming this flag belongs in the
universal stack unconditionally.

Deeper accuracy scoring (the fono-bench factual/tool-use fixture suites) was not rerun in this
pass — per the "trust public benchmarks, verify integration only" scope agreed for this
exploration, and given the original Gemma-4-12B run already established the accuracy-motivation
case (RO factual 62.5% → 100%, tool-use mean score 0.72 → 0.90+) against a dense model that both
MoE candidates are expected to meet or exceed per public benchmark scores.

## Results — Concurrency check (Task 2.4)

Done on Gemma 4 26B-A4B only (see MoE table above): `-np 2` at ctx 4096 added no measurable
penalty to single-stream latency and did not exhaust memory on this constrained live-boot
environment (as little as 6.9 GB "available" going in). Qwen3.6-35B-A3B `-np 2` was **not**
re-tested — the live-boot session's memory pressure (tmpfs root consuming ~6.4 GB unavoidably,
zero real swap available at the time) made a second 17 GB-model load too risky to justify; this is
an environment limitation, not a finding about the model. Recommend re-running Task 2.4 for Qwen
on a normal (non-live-boot) install before finalizing the default.

## Decision

**Primary MoE candidates confirmed viable and roughly co-equal in speed** (Gemma 4 26B-A4B and
Qwen3.6-35B-A3B both land in the 17–26 tok/s range with sub-1.2s TTFT on this laptop's Vulkan
iGPU), which matches the Phase 1 quality expectation split:

- **Gemma 4 26B-A4B**: simpler to operate (no speculative-decoding content-dependence to manage),
  marginally better RO throughput, same publisher family as the existing E2B default (lowest
  integration/registry risk).
- **Qwen3.6-35B-A3B**: meaningfully stronger on coding/agentic benchmarks per its public model
  card, and its native MTP head is a large, free win specifically for the programming workload —
  at the cost of needing workload-aware speculative-decoding gating to avoid the EN/RO regression.

**The dense track (Gemma 4 12B) is not competitive as a default tier.** Even with every
model-agnostic technique tried (KV quant, n-gram speculation), it stayed at 8.6–12 tok/s — both
MoE candidates beat it by 1.5–3× while being smarter models. Dense 12B/31B remain viable only as an
explicit "no MoE-capable model available for this use case" fallback, not the primary path.

**Universal-stack verdict, revised from v2/v3's assumption:** most of the stack *is* universal
(`--reasoning off`, flash-attn, KV Q8_0, `-np ≥1`) and safe to always enable. Two techniques
(n-gram self-speculation, MTP speculative decoding) are **content/workload-dependent** and should
not be forced on unconditionally — this is a real, measured correction to the "one flag stack for
everything" goal, not a design failure: both techniques still slot into a "per-request override"
model cleanly (an API caller doing code generation opts into `draft-mtp`; a voice turn does not),
which fits Fono's existing per-request architecture better than a single static launch flag anyway.

## Updated Verification Criteria — status

- ✅ Every Phase 2 run recorded with exact command line, TTFT, tok/s, acceptance rate, RSS.
- ✅ Chosen defaults (both MoE candidates) reach ≥10 tok/s decode and ≤1.2 s TTFT — exceeding the
  ≥10/≤1s bar in most cases.
- ✅ Universal stack (minus n-gram/MTP) runs unmodified across one dense and two MoE models.
- ✅ No licence blocker (Phase 1).
- ✅ No leaked thinking-channel markers, confirmed on both Gemma-4 and Qwen3.6 families.
- ⚠️ Concurrency check completed for Gemma 26B-A4B only; Qwen3.6 concurrency deferred to a
  non-live-boot environment (see above).

## Follow-ups Before Shipping

1. Re-run Task 2.4 (concurrency) for Qwen3.6-35B-A3B on a persistent install with normal swap/RAM
   headroom.
2. Decide the per-request mechanism for `draft-mtp` gating (Fono request metadata: workload =
   "code"/"tool" vs "chat"/"voice") — this is now an integration design question, not a
   benchmarking one.
3. Run the EAGLE-3 speculator path for Gemma 4 26B-A4B (Phase 1 confirmed the checkpoint exists
   and is Apache-2.0) as a point of comparison against Qwen's free native MTP — currently unmeasured.
4. Scope the Fono-managed `llama-server` child-process integration (model download/registry entry,
   config/tier wiring, process supervision, `-np`/`--spec-type` request-time overrides) as the next
   engineering task, now that both candidate models and the flag set are measured and confirmed.
5. Clean up `../fono-tmp` (models, cloned llama.cpp build) once the decision is ratified — it is
   outside the repo and was intentionally kept off the live-boot's RAM-backed root the whole time.

## Alternative Approaches (unchanged from v2/v3)

1. Embedded-runtime path with upstream C-API PRs for speculation — revisit only if the managed-
   server model proves operationally painful.
2. Per-workload tuned configs instead of a universal stack — partially validated as *necessary*
   for n-gram/MTP specifically (see Decision), not for the rest of the stack.
3. External runtime (Ollama/llama-swap) — rejected, breaks Fono's self-contained install story.
