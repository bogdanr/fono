# Larger-than-RAM LLM Inference for Fono — Unified Research Plan (v4)

**This plan supersedes and merges** `2026-07-21-fono-larger-than-ram-llm-research-v3.md`
(the original campaign, Phases 0–6) and
`2026-07-21-fono-larger-than-ram-llm-research-next-v1.md` (the follow-on campaign,
Phases 7–12). It carries forward every completed result from v3, folds in the
forward-looking phases from next-v1, and **re-prioritises the whole effort around
the DwarfStar (`github.com/antirez/ds4`) evidence** of what actually delivers major
wins. Read this file; the two sources are retained only for history.

## Objective

Unlock the capability for Fono to run models **larger than available RAM** at
**acceptable interactive speeds**, keeping the resident floor (attention, router,
embeddings, shared experts) in fast memory and streaming the cold routed-expert
mass from SSD. Evaluate every technique that makes sense — stock `llama.cpp`
knobs, OS tricks, source-level fork modifications, quantization, the iGPU path,
and speculation — in the isolated workbench at `../llm-testing`
(`/mnt/nvme0n1p5/Work/llm-testing`).

Deliverables: (1) a reproducible **machine-independent** harness; (2) a ranked
table of techniques by tokens/sec-per-byte-of-RAM **and quality**; (3) a concrete
Fono integration proposal respecting the binary-size budget and Fono's per-role
(polish vs assistant) model-selection design.

## Status snapshot (what is already settled)

**Proven — the core mechanism works.** With a `GGML_CPU_REPACK=OFF` build,
`mmap` gives a graceful larger-than-RAM streaming band on two MoEs
(gemma-4-26B-A4B, Qwen3.6-35B-A3B): usable interactive decode down to ~40 %
resident, no OOM. The **stock default build cannot do this** — it repacks Q4_0
into non-evictable anonymous RAM and force-disables mmap, OOMing in seconds under
any cap below model size. **Shippable finding #1: build without repack + mmap on.**

**Three "read/compute cleverer" levers tested NEGATIVE** (all in `notes/phase4*.md`):
1. **Async expert prefetch** (`MADV_WILLNEED`, Task 4.1): ~3–6 % *slower*. MoE
   decode is compute-light, kernel readahead already fetches each contiguous
   ~3 MB expert block, and tight caps are reclaim-bound — no I/O to hide.
2. **Managed hot-expert cache vs OS LRU** (Task 4.2): a statically/calibration-
   derived pinned cache is **2.5× worse than plain OS-LRU** on the CPU+mmap path,
   because expert selection is strongly input-dependent (only 20–40 % hot-set
   overlap across inputs) and the OS page cache already adapts online, within
   ~25 % of an oracle.
3. **E-core straggler / thread affinity**: `-t 8` (all 4P+4LPE cores) beats
   P-core-only pinning; llama.cpp's atomic work-stealing already neutralises the
   barrier skew.

**Residual axiom (drives everything below):** throughput is **bandwidth-bound and
already near the achievable serial rate** on the CPU+mmap path. The only levers
left are: **(A) read fewer cold bytes per token** (quantization — untested, and
per DS4 the single biggest win), **(B) raise effective memory-bandwidth
utilisation** (batch-1 decode uses only ~45–50 % of practical LPDDR5X — the iGPU's
latency hiding is the classic fix), and **(C) produce more tokens per weight pass**
(speculation — DS4 reports marginal). Latency-hiding on the CPU path is dead.

## DwarfStar (DS4) reconciliation — why our negatives and DS4's wins agree

DS4 is a live, high-quality reference for exactly this problem (DeepSeek V4
Flash / GLM 5.2 streaming on Mac/CUDA/ROCm). It is essential to understand **why
its shipped tricks do not contradict our negatives**, so we chase the right wins:

- **DS4's managed expert cache + static hotlist lives in GPU-visible memory
  (Metal/CUDA/ROCm), where there is NO OS demand-paging.** On that path a managed
  cache is the *only* paging mechanism — it is not competing with an OS page
  cache, so it cannot "lose to LRU" the way it does on our CPU+mmap path. DS4's
  80 %-of-working-set sizing and "reserve two full routed layers" headroom are
  **OOM-safety** accounting for unified memory, not a claim of beating LRU. → Our
  Task 4.2 negative stands **for the CPU path**; the managed cache is re-scoped in
  this plan to the **iGPU path** (Phase 8), where it becomes necessary again.
- **DS4's actual major win is asymmetric imatrix routed-expert quantization**, not
  the cache: routed experts at `IQ2_XXS` (up/gate) + `Q2_K` (down) — i.e. ~2-bit —
  while shared experts, attention, router, and embeddings stay `Q8/F32`. Routed
  experts are the majority of model bytes (our profiling: **87–89 %** of the
  streamed mass), so 2-bit routed quant roughly **halves cold_bytes/token** at
  *verified* quality (DS4 gates every quant on a 100-case fixture). **This is the
  untested lever most likely to be a major win, and it applies to BOTH the CPU and
  iGPU paths.** It becomes Phase 6, the top priority.
- **DS4 proves 2-bit routed quant is safe only with a quality gate.** DeepSeek/GLM
  "tolerate aggressive routed quantization"; our models (gemma-4-26B, Qwen3.6-35B)
  may not tolerate it equally. Hence a **quality harness is a prerequisite**
  (Phase 5) — byte savings are never scored in isolation.
- **DS4's speculation (DSpark/MTP) is honestly reported as marginal.** We treat it
  the same: measure net-of-residency, keep behind a flag (Phase 9).
- **DS4's distributed / tensor-parallel / multi-GPU serving modes are out of scope**
  for Fono's single-desktop single-binary ethos. Useful only as a reference
  ceiling (Phase 10).

## The major-wins thesis (ranked by expected value)

1. **Asymmetric 2-bit routed-expert imatrix quant (Phase 6).** Biggest, universal
   (CPU + iGPU), directly halves the dominant cold-byte term. Gated on quality.
2. **iGPU decode for utilisation (Phase 8).** Batch-1 decode is latency-bound at
   ~50 % BW util on CPU; a GPU hides latency and can push toward the ~70 GB/s
   ceiling (≈2× decode) on the *same* shared memory — directly answers the user's
   "faster on an iGPU laptop?" question. Valuable **independently of streaming**:
   a faster fits-in-RAM point enables the smaller-model-faster trade per role.
   The managed expert cache (reconciled from Task 4.2) belongs here.
3. **Quant × cache/util interaction (Phases 6–8).** Smaller experts ⇒ more fit
   resident ⇒ fewer misses; the levers multiply.
4. **Speculation (Phase 9).** Honest, likely marginal; measure before betting.

## Governing model

```
time/token ≈ (cold_bytes_from_ssd / SSD_BW_eff)
           + (hot_bytes_from_ram  / (RAM_BW * util))
tokens_out/sec ≈ accept_factor / (time/token)
```

Attack surfaces map to phases: shrink `cold_bytes_from_ssd` (Phase 6 quant; Phase 7
OS tricks), raise `util` toward 1.0 (Phase 8 iGPU), raise `accept_factor` above 1.0
(Phase 9 speculation). Every task is judged first by **cold_bytes/token** (the
portable, ±2 % metric) and — new since v3 — by a **quality score**, since tok/s on
the reference laptop is power/thermal-bound (±40 %) and unsuitable for fine deltas.

## Scope guards

- **Not tuned to one machine or model.** The rig is a reference only; RAM is
  *emulated* via cgroups; the headline metric is cold_bytes/token, from which tok/s
  on any SSD/RAM combo is predicted. Rig constants: practical RAM BW ~75 GB/s,
  SSD ~14 GB/s (≥1 MB reads).
- **License is out of scope for the concept phase** (models chosen on technical
  merit, encumbered weights allowed). The license/productization check is deferred
  to Phase 11.
- **Binary size is the top project priority.** Prefer C/C++-side fork changes over
  new Rust deps; any binary-affecting change gates on `./tests/check.sh
  --size-budget`. iGPU (Vulkan/SYCL) is an **optional feature/build**, measured
  against the budget before any ship decision.
- **Dense larger-than-RAM is out of scope.** Dense decode touches every weight
  every token, so streaming gives `tok/s ≈ SSD_BW / non_resident_bytes` — ~0.5–1.4
  s/token even at mild over-RAM ratios, with no hot set to cache and no iGPU help
  for the SSD term. Only MoE can stream experts. Dense models remain the
  fits-in-RAM tier (small dense on iGPU), covered as a control in Phases 1/8.
- **Cold/warm is a spectrum bracketed by two endpoints.** Every config is reported
  at both: cache-drop-before-run (worst realistic case — desktop use evicted the
  warm set) and repeat-without-drop (best case — warm steady state). Degradation
  between them is monotonic on the mmap path (Phase 2 band), so intermediate
  warmth interpolates; no synthetic co-tenancy loads are simulated.

## Fono model-selection design this must respect

Fono selects local LLMs **per role** (polish/F7, assistant/F8). The registry
`shared_model()` (`crates/fono-core/src/llama_backend.rs:76-97`) already implements
the intended rule: **same file for both roles → one shared `Arc<LlamaModel>`**
(single mmap/resident set); **different files → two independent loads**. The
`LlamaBackend::init()` singleton (`:42-52`) is a separate hard constraint (second
init panics) and stays single always.

**Caveat this project touches:** the cache key is the canonical path and reuses the
**first-loaded `LlamaModelParams`** (`:67-72`). Harmless today (both roles load
`default()`), but once per-role offload/quant/iGPU knobs exist, "same file with
different params" would silently share the first variant — the key must then
include the offload-relevant params (Phase 11).

## Implementation Plan

### Phase 0 — Workbench & machine-independent harness — DONE

- [x] Task 0.1. Scaffold `../llm-testing/` (`models/ runtimes/ harness/ results/
  notes/`) + README with the fixed methodology.
- [x] Task 0.2. Baseline `llama.cpp` CPU build in `runtimes/` (commit `76f46ad`).
- [x] Task 0.3. **RAM emulation** via cgroup v2 (`harness/ram-run.sh`: `memory.max`
  + swap off, reports disk bytes via `io.stat`, peak RSS, OOM, wall). Validated
  (300 MiB read → 315 MB reported).
- [x] Task 0.4. **Metrics collector** (`harness/run-bench.sh`): decode/prefill
  tok/s, RSS, faults, disk read bytes, derived **cold_bytes_per_token**; rig
  RAM_BW/SSD_BW probes in `harness/hw-probe/`. `COOLDOWN=<s>` between repeats
  (thermal isolation).
- [x] Task 0.5. Fixed workloads (polish/assistant/longctx), fixed seed + token
  count, cache drop between runs, median + spread.

### Phase 1 — Model & workload matrix (technical merit) — PARTIAL

- [ ] Task 1.1. Span the space: total/RAM ratio (1.2×/2×/4×), active-param ratio
  (dense control vs sparse MoE), expert count/size.
- [x] Task 1.2. Motivating targets acquired + streamed: **gemma-4-26B-A4B**
  (gemma4, QAT Q4_0, 14.4 GB), **Qwen3.6-35B-A3B** (qwen35moe, IQ4_XS, 17.7 GB).
  Ornith-1.0-35B sized (MIT, qwen3_5_moe→qwen35moe, ~21 GB Q4) but not downloaded;
  397B-class deferred.
- [ ] Task 1.3. Multiple quantizations per model (Q4_K_M, IQ3, IQ2, Q2_K) + KV
  quant — now largely subsumed by Phase 6 (the quant campaign).
- [x] Task 1.4. Static profiles recorded: gemma-4-26B = 89.1 % expert-FFN / 10.9 %
  resident, per-expert 1.06–2.18 MB (fast read regime); Qwen3.6-35B = 87.2 % /
  12.8 %, per-expert sub-tensors 544 KB (just below 1 MB — flag for read-size).
- [ ] Task 1.5. Two role-deployment **scenarios** tested throughout: (A) **shared**
  — both roles same over-RAM model (one resident copy); (B) **distinct** — two
  models concurrently (split RAM budget + I/O contention).
- [ ] Task 1.6. **Acceptance thresholds** (proposed: polish ≥ ~10 tok/s, assistant
  ≥ ~5–8 tok/s), evaluated under both scenarios. *User confirmation still open.*

### Phase 2 — Stock llama.cpp knobs — DONE

- [x] Task 2.1. mmap baseline sweep — **KEY FINDING**: default `GGML_CPU_REPACK=ON`
  repacks Q4_0 to anon RAM + disables mmap → OOM under any cap < model size. Build
  `-DGGML_CPU_REPACK=OFF` → file-backed mmap → graceful band (gemma 19.5→1.5 t/s
  fits→3 G; ~7 t/s at 40 % resident).
- [x] Task 2.2. `--n-cpu-moe`/`-ot`: **CPU↔GPU no-ops on a CPU build**; the
  RAM/SSD split comes from mmap + OS page cache.
- [ ] Task 2.3. Anti-pattern controls (`--mlock` over-RAM; `n_gpu_layers` w/ GPU) —
  fold the GPU part into Phase 8.
- [x] Task 2.4. Quant + KV-quant + batch/threads: flash-attn + KV-quant within
  noise (KV already tiny via SWA+GQA); `-dio` OOMs (non-mmap path). No stock free
  lunch for this model class.
- [x] Task 2.5. Ranked stock configs (scenario A): tok/s power/thermal-bound
  (±40 %); cold_bytes/token stable (±2 %). Ship-today = no-repack + mmap. Scenario
  B still TODO (moves to Task 1.5 coverage).

### Phase 3 — llama.cpp source mods (fork) — clever-IO levers CLOSED

- [x] Task 3.1 (was 4.1). Async expert prefetch — **NEGATIVE** (see snapshot).
  Fork branch `phase4-expert-prefetch`, commit `035ec7b08`.
- [x] Task 3.2 (was 4.2). Managed CPU-path expert cache vs OS LRU — **NEGATIVE, do
  not build on CPU** (see snapshot + reconciliation). Instrumentation
  `GGML_EXPERT_LOG` committed (`4dd3348be`) — reused in Phases 5/6/8.
- [~] Task 3.3 (was 4.3). Speculative expert loading from router logits —
  **deprioritised** (fails for the same compute-light reason as 3.1). Revisit only
  if Phase 8 changes the compute/IO balance.
- [→] Task 3.4. Expert-friendly on-disk layout (≥1 MB aligned blobs) — **merged
  into Phase 6** (layout is produced by the quant/repack tooling there). Relevant
  for Qwen3.6's 544 KB sub-tensors.

### Phase 4 — OS / filesystem tricks — OPEN (lower priority)

- [ ] Task 4.1. Readahead tuning (`read_ahead_kb`, `blockdev --setra`).
- [ ] Task 4.2. madvise policy probe (`MADV_RANDOM`/`WILLNEED`/THP via shim).
- [ ] Task 4.3. **Compressed RAM tier (zram/zswap)** — a middle tier between RAM
  (~75 GB/s) and SSD (~14 GB/s); most relevant to scenario B. Trades CPU for
  effective RAM, needs no model changes.
- [ ] Task 4.4. O_DIRECT loader vs page-cache mmap; confirm model on fastest device.
- [ ] Task 4.5. Re-rank OS tricks on the Phase 2 Pareto front.

### Phase 5 — Build `fono-benchmark`: the unified scorecard harness (prerequisite for Phase 6)

**What it is.** One tool, one command: point it at a model (+ a config + a technique
being tested) and get back a **single scorecard row** covering every axis of the
decision — *can it do the job* (capability), *what does it cost to run* (performance
& resources), and *what were we testing* (experiment annotation). This is the
measurement instrument for the whole rest of the project: every later phase (quant,
scenario B, iGPU, speculation) produces its evidence by running models through
`fono-benchmark` and comparing scorecards. Lives in `../llm-testing` for now; may
later graduate into a user-facing "which model fits my machine?" helper — but that is
a Phase-11 productization decision, kept out of the shipped binary until then.

**Framing correction (supersedes the earlier drift/KL design).** We do NOT gate on
fidelity-to-the-full-precision-model. The full-precision model is unshippable on the
target machine, so "how far did the quant drift from it" answers the wrong question.
The gate is **absolute task success**: given the model that actually fits and runs,
*can it still do the work?* (A cheap KL/perplexity check MAY be kept as an optional
early tripwire, but it is not the decision metric.)

**Design invariant that makes it affordable — measure quality once, join to the perf
sweep.** A model's *outputs* depend only on `(model, quant)`; RAM cap, thread count,
build (repack vs no-repack, a lossless layout change), and SSD streaming change only
*speed*, never the tokens produced. So the expensive capability suite runs **once per
`(model, quant)`** and is **joined** to the cheap per-config performance sweep on that
key. No re-running Opus for every RAM cap.

- [x] Task 5.1. **Performance & resource meter.** *(DONE — `harness/steady-cold.sh` (two-length steady-state) + RAPL energy in `ram-run.sh`.)*
  New `steady-cold.sh` runs the same config at two decode lengths (N1<N2) and takes
  `Δrbytes/(N2−N1)`, **cancelling the one-time load+prefill** the Phase-0 single-length
  metric conflated. **Major correction:** the old single-length `cold_bytes/tok` was
  inflated ~8–11×. gemma-4-26B @6G steady-state: **v1 = 66 MB/tok (was 539), v1.5 =
  39 MB/tok (was 439)** — *ranking and every Phase-6 verdict preserved* (v1.5 still
  −41% vs v1). Physical finding: at 6G (≈55–60% resident) SSD is ~2–2.5% utilised —
  decode is **fault/eviction-latency-bound, not bandwidth-bound** at moderate over-RAM
  ratios (explains why prefetch 4.1 found nothing to overlap). RAPL package energy
  (`/sys/class/powercap/intel-rapl:0`, wraparound-handled) surfaced as
  `ss_energy_j_per_tok`. Remaining (deferred): warm+cold pair endpoints, TTFT, and
  wiring the steady meter into the main `fono-bench` sweep.
- [ ] Task 5.2. **Portable RAM emulator (mlock fallback).** DS4-style
  `--simulate-used-memory` (chunked `mmap`+`mlock`) alongside the cgroup scope, so
  the over-RAM regime is forceable without root / on non-Linux (de-risks
  macOS/Windows).
- [x] Task 5.3. **Capability suite — absolute task success (the quality axis).** *(DONE — `capability.py` + `judge.py`; validated on gemma-4-26B: coding 8/8, polish 0.98.)*
  Graded two ways, objective wherever possible:
  - [x] **5.3a — Objective, machine-graded coding tasks (the backbone).** *(DONE — 8 tasks trivial→hard, run-the-code grading in an isolated `-I` subprocess.)* Give the
    model a small programming task; **execute its output against hidden unit tests**
    in a sandboxed subprocess (timeout, no network); score pass/fail (pass@1, greedy).
    No judge, no bias, deterministic, no API key. Span a **difficulty range** (trivial
    → moderate) so the scorecard reveals *where each shrink level falls off the cliff*,
    not just a blunt pass rate. Tasks slightly novel/mutated to test reasoning, not
    memorisation.
  - [x] **5.3b — Subjective tasks graded by a frontier judge (Claude Opus).** *(DONE — `judge.py`; polish scored deterministically via difflib, assistant via absolute rubric, API-key-gated on `ANTHROPIC_API_KEY`.)* For
    open-ended assistant answers, dictation-polish, and non-coding work, send task +
    model answer to a big hosted judge with an **absolute rubric** (correct? helpful?
    user-acceptable yes/no) — judged **on its own merits, not vs the full model**.
    API-key-gated; used only where machine-grading can't apply, to keep cost/noise
    down. Where polish has a clean target, prefer deterministic edit-distance scoring
    over the judge.
  - [ ] **5.3c — Coverage & scale.** *(PARTIAL — pipeline proven with a seed set: 8 coding + 2 polish (en/ro) + 2 assistant; still need to scale to ~100 cases.)* ≥~100 cases spanning coding (objective),
    general/instruction-following (judge), polish (deterministic where possible), and
    the supported spoken languages. Greedy/temp-0, fixed seeds/prompts for
    reproducibility.
- [x] Task 5.4. **Experiment-annotation layer (the ledger).** *(DONE — `results/runs.csv` + `--technique/--comment/--verdict/--build` on `fono-bench`; verdict is a fixed enum, comment is a CSV column.)* Every scorecard row
  carries: a free-text **comment** (the technique/hypothesis, e.g. "no-repack + mmap
  streaming"), a short **technique tag** (to group all runs of one experiment), full
  **build provenance** (llama.cpp commit + branch + the flags that matter, esp.
  repack on/off), **active env toggles + full command line**, date/host, and a
  fixed-vocabulary **verdict** (`win` / `neutral` / `negative`) + a pointer to the
  notes file. This makes the benchmark self-documenting and captures **negative
  results in the same place as the data that proves them** (prefetch, thread-affinity,
  static expert cache — and every future dead end). Comment/verdict are proper
  columns (spreadsheet/plot-safe), verdict is an enum.
- [x] Task 5.5. **Scorecard aggregator & deliverable.** *(DONE — `aggregate.py` joins on model filename, emits `cap_per_gib_ram` + `usable`, labels PORTABLE/MACHINE/DERIVED; validated on gemma scorecard.)* Join capability (per
  `model,quant`) to the perf sweep (per `model,quant,config`) plus the annotation
  layer → one unified scorecard (CSV+JSON). Emit the **derived decision numbers**:
  capability-per-GB-resident, capability-per-cold-byte, tok/s-per-GB, and a plain
  **usable-on-this-machine** verdict (fits AND fast-enough AND good-enough).
  **Resident-floor accounting:** the `cap_per_gib_ram` denominator is the true
  resident cost — floor (attention/embeddings/shared experts/router) + KV at the
  workload's context + minimum viable expert cache — NOT model size on disk;
  streaming makes routed experts cheap in RAM but the floor is incompressible and
  scales with the model. Label
  every field **portable** (capability, cold_bytes/token) vs **machine-specific**
  (tok/s, energy) so speed numbers are never quoted as universal. Keep axes explicit;
  no single collapsed "Fono score" (it hides the tradeoff) — the **Pareto front**
  (capability × cold_bytes/token × RAM) is the output.
- [x] Task 5.6. **Router/expert-activation tracer** — DONE.
  `GGML_EXPERT_LOG_COMPACT=1` (fork `build-prefetch`) emits a compact numeric line per
  layer `<layer> <expert_ids…>` (gate/gate_up only — skips the redundant up/down that
  select identical experts; works for gemma's fused `gate_up` and qwen's separate
  `gate`). Token boundaries implicit (col-1 resets to 0). Validated: 6-token trace →
  30 layers, 1508 unique (layer,expert) pairs = 39.3% of 3840 slots; approaches full
  coverage over a real corpus. Feeds imatrix-calibration coverage + future
  speculative-loading validation.

### Phase 6 — Reduce cold bytes/token via quantization (THE major win, quality-gated)

- [x] Task 6.1. **Asymmetric imatrix expert quant (DS4 recipe).** DONE (gemma-4-26B).
  Source = non-QAT Q8_0 (26.9 GB); imatrix from bartowski calibration_datav3 (100×512,
  disjoint from tasks). gemma-4 fuses gate+up → `ffn_gate_up_exps` + `ffn_down_exps`.
  **Result: thesis CONFIRMED.** v1 (gate_up IQ2_XXS / down Q4_0, 10.8 GB) holds quality
  (coding 9/10, polish 0.972) at **−33% cold_bytes/token @6G** → 7.4 vs 6.8 t/s.
  v2 (down→Q2_0, 8.66 GB) hits **−53% cold bytes, 11.9 t/s (1.75×)** but coding
  **collapses to 3/10** — the quality cliff. See notes/phase6.md, scorecard-phase6-asym.csv.
- [ ] Task 6.1-orig (original spec, kept for reference). Quantize only
  routed experts (up/gate `IQ2_XXS`, down `Q2_K`) while keeping
  attention/router/shared-experts/embeddings at `Q8/F32`; build the imatrix from a
  build the imatrix from a
  calibration corpus (reuse the Phase-5.6 traces + a DS4-style dataset). Measure
  cold_bytes/token AND the 5.3 capability score vs the uniform Q4_0/IQ4_XS baselines,
  at streaming caps, if quality holds. Use llama.cpp's quantiser in `runtimes/`;
  produce the GGUFs offline in the workbench. **All quants derive from the 5.3a
  F16 base and are scored against it — not from the QAT Q4_0 (which has no
  higher-precision reference).**
- [ ] Task 6.2. **Per-layer mixed precision.** "Last-K layers at higher precision"
  (DS4 `q2-q4-imatrix`) and any layer-sensitivity ordering from 6.1's imatrix.
  Cheap quality recovery for a small byte cost.
- [ ] Task 6.3. **Expert-friendly on-disk layout** (absorbed Task 3.4). Ensure each
  expert is a single ≥1 MB aligned blob to stay in the 14 GB/s read regime;
  especially check Qwen3.6's 544 KB sub-tensors don't fragment into slow reads.
- [ ] Task 6.4. **Quant × cache/util interaction.** Smaller experts ⇒ more fit a
  fixed budget ⇒ higher hit rate / more resident. Re-run the streaming band at
  each quant; report the joint Pareto front (cold_bytes/token × quality × RAM).
- [x] Task 6.5. **Do our models tolerate 2-bit routed quant?** DONE (gemma-4-26B): YES
  with a smart imatrix 2-bit; the earlier cliff was a *format* accident, now fixed.
  Progression: v1 (IQ2_XXS gate_up + Q4_0 down, 10.8 GB) = 9/10 coding; v2 (down→crude
  Q2_0, 8.66 GB) = 3/10 (cliff); ablation variant B (gate_up→crude Q2_0) = **0/10** →
  proved **FORMAT dominates, not bit-count** (same ~2 bits, smart i-quant fine / crude
  legacy quant destroys the model). Root cause: gemma's `ffn_down_exps` inner dim **704
  is not ÷256**, so Q2_K/IQ2_* can't tile it → silent fallback; only crude block-32/64
  types fit. **FIX BUILT & CONFIRMED — v1.5:** zero-pad routed inner dim 704→768 (bump
  `gemma4.expert_feed_forward_length`) so Q2_K tiles `down`; numerically exact (padded
  Q8 = byte-identical output), pure GGUF rewrite, no kernel change
  (`harness/pad-expert-ffn.py`). Result: **9.60 GB, coding 9/10, polish 0.969, −45% cold
  bytes & 1.54× decode @6G vs QAT Q4_0** — v1.5 Pareto-DOMINATES both v1 (smaller+faster,
  same quality) and v2 (recovers 3/10→9/10 at ~90% of v2 speed). Sensitivity order:
  gate_up > down. See notes/phase6.md "v1.5 — the padding fix".
- [x] Task 6.5b. **Second-model generalization (Qwen3.6-35B-A3B).** DONE. Confirms the
  recipe transfers to a different architecture. Qwen's routed experts are **separate**
  gate/up/down (not gemma's fused gate_up) and all natively **÷256** (down=512,
  gate/up=2048) → smart imatrix i-quants (Q2_K/IQ2_XXS) tile with **NO padding needed**;
  gemma's 704 problem was arch-specific. Asym quant (gate/up IQ2_XXS + down Q2_K, shared
  experts Q8_0) = **11.74 GB, coding 9/10** (matching the IQ4_XS 4-bit baseline and gemma
  v1/v1.5), polish intact, coherent. Universal rule holds: **sub-4-bit routed quant must
  use imatrix i-quants; pad non-÷256 tensors to keep them off crude legacy quants.** All
  four surviving models re-benchmarked with the steady-state meter into
  `scorecards/scorecard-final-unified.csv`. See notes/phase6.md "Qwen3.6 generalization".
- [ ] Task 6.5-orig. **Do our models tolerate 2-bit routed quant?** Explicit go/no-go
  per model against the 5.3 quality tolerance; if a model degrades, fall back to
  IQ3/mixed. This is the DS4 caveat made into a gate. **Imatrix-coverage gate
  (protects the verdict from a bad imatrix):** in an MoE, experts only accumulate
  imatrix statistics on inputs that route to them, so a low-coverage corpus
  silently mangles the untouched experts and would frame the *model* for a
  *calibration* failure. Before any negative verdict: (1) trace the calibration
  run itself (5.6 `GGML_EXPERT_LOG`) and report per-expert activation counts —
  near-zero-coverage experts mean the imatrix is blind there; (2) if coverage is
  poor or the quant fails 5.3, re-run once with an upgraded corpus (more chunks +
  code + supported-language slices) — if capability jumps, the imatrix was the
  bottleneck; only if it stays flat is "model doesn't tolerate 2-bit" a real
  verdict.
- [~] Task 6.6. **Generic-recipe rungs vs curated (the model-churn experiment).**
  Models change faster than curation; quantify what zero/low-curation gives up.
  **TOOLING BUILT (2026-07-22):** `harness/make-streamable.sh` — the consolidated
  offline authoring driver. Auto-detects routed-expert topology by name pattern
  (`ffn_*_exps`), handling both fused (`gate_up`, gemma) and separate
  (`gate`/`up`/`down`, qwen) layouts; auto-pads any routed reduction dim not
  ÷256 to the next multiple (bumps arch FFN-length metadata) and skips padding
  when already aligned; chains imatrix → asymmetric quant (routed → imatrix
  i-quants only, never legacy Q2_0/Q3_0; attention/router/shared-experts/embed
  stay high-precision); emits the streamable GGUF + a `.recipe.json` sidecar
  (per-tensor types, will_pad flags, expected steady cold-bytes/token). Dry-run
  validated on both gemma (→ v1.5 recipe: fused gate_up=iq2_xxs, down=q2_k+pad)
  and qwen (→ qwen-asym recipe: separate gate/up=iq2_xxs, down=q2_k, no pad);
  generated `--tensor-type` flags match the hand-run validated recipes exactly.
  Shape decision: **offline workbench/repo script, NOT a `fono` subcommand** —
  quantization is one-time author-side work; linking the quantizer/imatrix/GGUF
  rewriter + Python into the shipped binary would violate the size budget for a
  runtime that never quantizes. If graduated, lands in the fono repo `scripts/`
  (alongside `gen-ort-models.sh`), never the binary. Runtime consumer side (mmap
  + no-repack load in `shared_model()`) is a separate Phase 11 item.
  **STILL TODO:** the actual three-rung A/B (generic-auto vs generic+imatrix vs
  curated) to decide the curated-tier size with data.
  Original spec:
  Three rungs on the same model, scored on the 5.3 suite + cold_bytes/token:
  (1) **generic-auto** — detect routed-expert tensors by name pattern
  (`ffn_*_exps`), quantize to the lowest imatrix-free safe level (Q3_K/IQ3-ish),
  rest Q8; works on any MoE GGUF on release day. (2) **generic-auto + generic
  imatrix** — same recipe with an imatrix from a model-agnostic corpus; still
  automatic, unlocks the 2-bit rungs (IQ2_XXS practically requires an imatrix).
  "Generic" means *generic but role-aware*: the standing corpus is prose (e.g.
  bartowski `calibration_datav3`) **plus a code slice and slices of Fono's
  supported spoken languages** — fixed once for all models, no per-model
  curation, but not blind to the workloads Fono actually routes at experts.
  Corpus stays disjoint from the 5.3 benchmark tasks. (3) **curated** — the
  hand-tuned, quality-gated 6.1 mix. If rung 2 lands within tolerance of rung 3,
  the curated tier shrinks to the shipped defaults and every new model gets rung
  1/2 automatically. This decides the curated-tier size with data.
- [x] Task 6.7. **Prefill under cap (TTFT for the assistant role).** DONE — built
  `harness/prefill-cold.sh` (two-length delta over *prompt* length, fixed tiny
  decode, cancels one-time model load; parses real prompt-token count from the
  verbose `prompt eval time = … / N tokens` line). Measured gemma-4-26B v1.5 and
  Qwen3.6-35B asym at cap 6G/4G. **Key finding: prefill cold-bytes per prompt-token
  is far LOWER than decode** — gemma 7.9 MB/prompt-tok @6G (11.6 @4G) vs decode's
  39.3 MB/tok; qwen 14.0 MB/prompt-tok @6G. Batching reuses each loaded expert
  across all prompt tokens, so long prompts do NOT explode SSD traffic — prefill is
  not the streaming bottleneck. No OOM at any cap. **So the DS4 double-buffered
  whole-layer streaming is NOT needed** (measurement-gated, and the measurement says
  don't build it). Recorded in `notes/phase6.md`; data in `results/prefill.csv`.
- [ ] Task 6.8. **Oracle imatrix (diagnostic only — never shipped).** Build one
  extra imatrix calibrated **on the 5.3 benchmark corpus itself** (deliberate
  test-set contamination) and produce the same 2-bit quant with it. Its 5.3 score
  is the **upper bound of what any imatrix could achieve** on our suite; the gap
  between it and the generic-imatrix quant (6.6 rung 2) measures exactly how much
  the calibration corpus matters for this model — separating "imatrix quality"
  from "model tolerance" with data. Small oracle−generic gap ⇒ corpus choice is
  a non-issue and rung 2 is safe; large gap ⇒ invest in the corpus (or per-model
  calibration) before blaming the model in 6.5. The oracle GGUF is a measuring
  stick only: contaminated by construction, excluded from scorecards' ship
  candidates and from any Fono deliverable.

### Phase 7 — Scenario B & role deployment (closes v3's biggest gap)

- [ ] Task 7.1. Scenario B under the best Phase-2/6 config: two concurrent models
  (polish + assistant) sharing one RAM cap. Measure thrash/OOM, per-model
  cold_bytes/token, and whether a **shared global budget** beats one-cache-each.
- [ ] Task 7.2. Verdict: viable under a shared budget, or **steer Fono to
  shared-model-only when either role is over-RAM** (surface the trade-off in
  config/docs). Note the **disjoint-resources split** as the likely winner:
  polish = small dense on iGPU (compute-bound, no SSD traffic) + assistant =
  streamed MoE on CPU (SSD/bandwidth-bound, near-idle CPU) barely contend; the
  hard scenario B is only two *streamed* models sharing one SSD.

### Phase 8 — iGPU path (promoted to first-class) + the reconciled managed cache

- [ ] Task 8.1. **CPU utilisation baseline & thread pinning.** Measure achieved
  GB/s at fits-in-RAM (≈33 GB/s of ~75 → ~45 % util). Thread sweep already done
  (`-t 8` wins; `notes/phase4.md`) — reuse as the honest CPU bar the iGPU must beat.
- [ ] Task 8.2. **Vulkan (and/or SYCL) iGPU decode at cap=max.** Primary
  deliverable: the **fits-in-RAM iGPU speedup curve across model sizes** —
  including **one dense control model** (e.g. a current dense ~8–14B; dense
  prefill is compute-bound, so the iGPU TTFT win should be largest there) —
  since that curve is what enables the smaller-model-faster trade per role.
  Offload all layers to the rig's Arc iGPU; measure achieved BW utilisation,
  decode tok/s, and TTFT vs the CPU baseline on the *same* LPDDR5X. Hypothesis:
  the iGPU hides latency and pushes util from ~50 % toward the ceiling (≈2×
  decode) with no extra bandwidth. iGPU × streaming composition is the secondary
  bet (8.3–8.5). Treat the backend as an optional build; **measure binary-size
  delta against the budget gate.**
- [ ] Task 8.3. **Managed expert cache in GPU-visible memory (reconciled Task 3.2).**
  On the iGPU path there is no OS page cache, so a DS4-style cache sized in *whole
  experts* (`cache_experts = (80% * budget − non_routed_bytes) / per_expert_bytes`)
  is the paging mechanism, not an LRU competitor. Implement/borrow it; A/B a
  cold-start LRU vs a **static hotlist warm start** (DS4 `ds4_streaming_hotlist`)
  — here the hotlist's job is warm-up avoidance, not beating LRU. Validate whether
  the input-dependence that killed it on CPU (Task 3.2) still bites on GPU.
- [ ] Task 8.4. **Tiered layer packing (iGPU + CPU + SSD).** DS4-style monotonic-
  contiguous placement (`ds4_layer_pack`): contiguous early layers resident in
  iGPU memory, resident floor + hot experts in fast memory, contiguous tail
  streamed from SSD. Measure vs pure-CPU streaming at matched caps.
- [ ] Task 8.5. **iGPU expert streaming feasibility.** Can experts page into
  iGPU-visible memory without a copy penalty on unified memory, and can the 8.3
  cache live in GPU-visible RAM? Determines if the cache and iGPU compose.

### Phase 9 — More tokens per weight pass (speculation)

- [ ] Task 9.1. **Speculative / MTP decode.** Wire a small draft model (or the
  model's MTP head where present, e.g. Qwen3.6/DeepSeek MTP) and measure
  accepted-tokens-per-target-pass and cold_bytes/**output**-token on the streaming
  path. DS4 reports this as marginal — measure honestly.
- [ ] Task 9.2. **Draft-model residency cost.** The draft contends for the RAM
  budget/cache; measure net-of-residency under a cap (can backfire).

### Phase 10 — External ceiling (reference only, not to ship)

- [ ] Task 10.1. Run **DwarfStar** (`make cpu`, and iGPU/ROCm where possible) and
  **ktransformers** on the same models/caps. Establishes how far a purpose-built
  engine gets and which tricks are worth porting to `bogdanr/llama-cpp-rs`.
  DwarfStar is DeepSeek/GLM-specific, so mainly a *technique* reference for our
  gemma/qwen targets, plus a possible opt-in backend (evaluated, likely
  reference-only per Fono's single-binary ethos).

### Phase 11 — Analysis & Fono integration

- [ ] Task 11.1. **Ranked results + plots**: cold_bytes/token and quality per
  technique; streaming bands; CPU vs iGPU utilisation; scenario A vs B; joint
  quant×cache Pareto. The decision artifact.
- [ ] Task 11.2. **Fono integration proposal.** Per-role knobs to expose in
  `shared_model()`/config: no-repack build flag, expert-quant variant (2-bit
  routed GGUF), optional iGPU offload + expert-cache byte budget + hotlist path.
  Which fork patches to upstream; projected **binary-size delta** (prefer C/C++;
  flag any Rust dep; gate on `./tests/check.sh --size-budget`; iGPU backend is an
  optional build).
- [ ] Task 11.3. **Registry cache-key change.** `shared_model()` must fold the
  offload/quant/iGPU-relevant `LlamaModelParams` into its key so (a) same file +
  same settings shares one copy (scenario A) while (b) same file + different
  per-role settings, or different files, each load separately (scenario B) —
  closing the "first-loaded params wins" gap (`llama_backend.rs:67-72`). Land with
  a regression test.
- [ ] Task 11.4. **Deferred license/productization check.** Once a technique wins,
  assess which shippable-default models satisfy Fono's rules (ADR 0004 / AGENTS.md);
  our self-built asymmetric-quant GGUFs may be redistributable where base weights
  allow, encumbered ones stay opt-in. gemma-4-26B-A4B (Apache-2.0) and
  Qwen3.6-35B-A3B (Apache-2.0) are both default-eligible on license grounds.
- [ ] Task 11.5. Size-budget path note: any shipped-binary change passes
  `./tests/check.sh --size-budget`; state projected impact so the cpu budget row
  (ADR 0022) is only revisited with sign-off.

## Verification Criteria

- Steady-state cold_bytes/token (5.1) reproduces within < 5 % median deviation,
  reported at both warmth endpoints (cold-start and warm) per config.
- Prefill tok/s and TTFT under streaming caps (6.7) are measured and reported;
  the overlapped-prefill mechanism is built only if plain-mmap prefill measures
  far below sequential SSD bandwidth.
- The generic-vs-curated rung comparison (6.6) yields an explicit decision on how
  much curation new models need (rung 1 / rung 2 / full curation).
- Any 6.5 negative verdict is backed by the imatrix-coverage gate: per-expert
  calibration coverage reported, and the oracle−generic imatrix gap (6.8) shown
  small enough that the corpus is not the culprit.
- Before the Phase 11 verdict, at least **one confirmation run at real 30–70 GB
  scale** on the reference machine validates that per-expert blob size and the
  absolute resident floor extrapolate from the small-model results.
- **Asymmetric 2-bit routed quant (6.1) achieves a materially lower cold_bytes/token
  at a quality score within an agreed tolerance of the uniform-quant baseline (5.3),
  confirmed on gemma-4-26B and Qwen3.6-35B.** (Primary success metric of the plan.)
- The technique is confirmed on at least one motivating large MoE (≥26B), not toy
  models — already satisfied for the mechanism; must be re-confirmed for quant.
- iGPU decode (8.2) gives a definitive, data-backed answer to "significantly faster
  on an iGPU?" — reported as achieved BW utilisation and tok/s vs the pinned-CPU
  baseline on the same memory, with the binary-size delta of the backend.
- The managed GPU-memory cache (8.3) is shown to be either necessary (no OS paging
  on that backend) and beneficial, or unnecessary — with data.
- Speculation (9) reported as cold_bytes/**output**-token including draft residency,
  with a ship/no-ship call.
- Scenario B (7) has a documented verdict: viable under a shared budget, or
  shared-model-only when over-RAM.
- The final proposal names concrete per-role knobs, the cache-key change, fork
  patches, and binary-size impact.

## Potential Risks and Mitigations

1. **Our models don't tolerate 2-bit routed quant as DeepSeek/GLM do.** Mitigation:
   Phase 6 is quality-gated (5.3); fall back to IQ3/mixed per model (6.5).
2. **Quality harness gives false confidence** (token-agreement ≠ capability).
   Mitigation: pair token-agreement with a rubric/task score; use held-out prompts.
3. **iGPU shares memory bandwidth, so gains may be modest.** Mitigation: 8.1 sets
   an honest CPU bar; report utilisation not just tok/s so a null result still
   informs.
4. **Vulkan/SYCL backend bloats the binary.** Mitigation: optional feature/build;
   measure size delta against the budget gate before any ship decision.
5. **Managed GPU cache re-inherits the input-dependence problem** that killed the
   CPU version. Mitigation: on GPU it is the *only* paging path, so the bar is
   "necessary + correct", not "beat LRU"; still validate hotlist hit-rate on
   held-out workloads and fall back to runtime LRU.
6. **Scenario B thrash/OOM.** Mitigation: measure under the cap; steer to
   shared-model-only if untenable (7.2).
7. **Speculation steals cache RAM and nets negative** (DS4 sees marginal).
   Mitigation: measure net-of-residency (9.2); keep behind a flag.
8. **Fork drift across quant-loader / cache / backend patches.** Mitigation: keep
   each patch small, independently toggleable, PR-ready (the fork already carries
   exactly one such patch).
9. **tok/s on the reference laptop is power/thermal-bound (±40 %).** Mitigation:
   rank on cold_bytes/token + quality; use cooldown-gated tok/s only as a coarse
   sanity check.

## Alternative Approaches

1. **Quantization-only campaign (Phase 6), skip iGPU + cache.** If 2-bit routed
   quant alone shrinks the pool enough to fit typical RAM budgets, this is the
   simplest ship and may be sufficient — the single highest-value path.
2. **iGPU-first for the iGPU-laptop segment.** If 8.2 shows the iGPU hits the BW
   ceiling on models that *fit* (post-quant), prioritise "make it fit in unified
   memory via quant" over SSD streaming for that hardware class.
3. **Ship stock no-repack + mmap only.** Already proven; zero further fork burden;
   the guaranteed fallback if Phases 6/8 underdeliver.
4. **Compressed-RAM tier only (zram/zswap, Phase 4.3).** Extends effective RAM with
   no model changes; helps scenario B most.
5. **Adopt DwarfStar as an external opt-in backend** rather than porting tricks —
   fastest route to its ceiling but conflicts with the single-binary/size ethos;
   evaluate in Phase 10, likely reference-only.
6. **Constrain to the shared-model (scenario A) case only** for over-RAM models,
   keeping distinct-model selection for models that fit in RAM.
