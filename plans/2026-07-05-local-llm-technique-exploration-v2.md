# Local LLM Technique Exploration v2 — Universal Stack on llama-server

## Objective

Serve the smartest viable local model on consumer laptop hardware (reference: Intel Lunar Lake iGPU,
Vulkan/UMA, 30 GB RAM) for all Fono workloads — voice assistant, smart home, programming, background
API tasks — by adopting a **single universal technique stack** on top of a Fono-managed `llama-server`,
rather than per-workload configurations. Deliverable: measured performance data (TTFT, tok/s, memory,
draft acceptance), a chosen default configuration (INI preset), and the productization path.

## Context and Prior Findings (v1 → v2 changes)

- **Everything is already in llama.cpp.** Verified in the local checkout (`../fono-tmp/llama.cpp`):
  draft/EAGLE-3/DFlash/MTP speculative decoding (`common/common.h:168-178`), n-gram self-speculation
  (4 variants), MoE offload (`--cpu-moe`/`--n-cpu-moe`), KV-cache quantization (`-ctk/-ctv`),
  grammar/JSON-schema constrained output, `--cache-reuse`, parallel slots, reasoning controls
  (`--reasoning off`, `--reasoning-budget`), router mode (`--models-dir`/`--models-preset`),
  `--sleep-idle-seconds`. No fork, no new engine, no upstream PR required for the core goal.
- **Ready-made EAGLE-3 speculators exist for our exact candidates**:
  `RedHatAI/gemma-4-26B-A4B-it-speculator.eagle3` and `RedHatAI/gemma-4-31B-it-speculator.eagle3`
  (`docs/speculative.md:39-40`). Requires GGUF conversion via `convert_hf_to_gguf.py --target-model-dir`.
- **SPEED-Bench** (`tools/server/bench/speed-bench`) measures TTFT/throughput/acceptance baseline-vs-
  speculative against a running server — replaces the custom harness tasks from plan v1.
- **Baselines measured on this laptop**: Gemma 4 E2B (RO factual 62.5 %, tool 0.72, p50 0.3 s) and
  Gemma 4 12B dense Q4_0 Vulkan (RO factual 100 %, tool 0.90+, 8.6 tok/s, TTFT ~0.7 s, pp 178 tok/s,
  host RSS ~2.9 GB with weights on iGPU/UMA).
- **Design principles from review**: (1) techniques are stacked universally, not per workload — even
  small benefits ride along; (2) the stack must work unchanged for dense and MoE models (all chosen
  flags are no-ops or graceful on the other shape); (3) quality evaluation is minimal (one smoke pass
  of existing fixtures as an integration tripwire — public benchmarks are trusted for intelligence);
  the measured axes are **TTFT, decode tok/s, memory (RSS + iGPU alloc), draft acceptance**.

## The Universal Stack (candidate defaults, to be validated)

Applied identically to every model; each item degrades gracefully when inapplicable:

| Flag | Purpose | Dense/MoE behaviour |
|---|---|---|
| `--spec-type ngram-mod` (via `--spec-default`) | model-free self-speculation, shared across slots | works for both; MoEs want long drafts |
| `--spec-type draft-eagle3` + speculator GGUF | high-acceptance drafting | layered on only when a speculator exists for the model |
| `--n-cpu-moe auto` / `-ot` overrides | expert offload | no-op on dense |
| `-ctk q8_0 -ctv q8_0` | KV cache compression | universal; `-ctv` requires flash attention on Vulkan — verify, else K-only |
| JSON-schema / grammar on tool calls (per request) | guaranteed-valid tool JSON | universal, API-level |
| `--reasoning off` (voice profile) / `--reasoning-budget N` (API) | no thinking-token latency or leaks | universal |
| `--cache-reuse 256`, `--cache-ram`, `--ctx-checkpoints` | prompt/KV reuse across requests | universal |
| `-np 2`, continuous batching (default) | API concurrency | universal; interacts with KV memory |
| `--models-preset` INI + `--models-max` + `--sleep-idle-seconds` | multi-model routing, idle resource release | universal serving layer |

## Implementation Plan

### Phase 1 — Desk checks (no downloads, ~half a day)

- [ ] Task 1.1. Licence-vet the specific artifacts: Gemma 4 26B-A4B QAT GGUF, the two RedHatAI EAGLE-3 speculators (weights licence, base-model licence), and any Qwen3.6-A3B GGUF alternative — against ADR 0004 criteria. Kill non-compliant candidates before spending bandwidth.
- [ ] Task 1.2. Verify Vulkan support on this build for: flash attention (gates `-ctv` quant), EAGLE-3/DFlash paths, MoE expert offload with UMA. Source: `docs/ops/Vulkan.csv` + a 5-minute smoke run each. Record what is disqualified on this hardware.
- [ ] Task 1.3. Check Python tooling availability for `convert_hf_to_gguf.py` (EAGLE-3 conversion needs HF checkpoints + target model dir); if the live-Linux lacks it, plan the conversion on another machine or find pre-converted GGUFs.

### Phase 2 — Measurement runs (SPEED-Bench + existing fixtures, ~1–2 days)

All runs via `llama-server` from `../fono-tmp/llama.cpp/build/bin/`, models in `../fono-tmp/models/`
(real disk, not tmpfs); record TTFT, decode tok/s, acceptance rate, peak RSS + iGPU allocation per run.

- [ ] Task 2.1. Dense track — Gemma 4 12B Q4_0: (a) stack-off baseline (have it: 8.6 tok/s), (b) + ngram-mod, (c) + KV quant + cache-reuse, (d) + EAGLE-3 speculator if the 31B speculator's smaller sibling exists or DFlash/draft alternative applies. Measure EN and RO prompts separately (acceptance is language-sensitive).
- [ ] Task 2.2. MoE track — Gemma 4 26B-A4B (Q4 first; asymmetric-quant artifact if available): (a) full-offload attempt, (b) `--n-cpu-moe` sweep (0/8/16/all) to find the UMA sweet spot, (c) + ngram-mod, (d) + EAGLE-3 speculator (exists for exactly this model). Record cold vs warm TTFT (expert page-cache effect).
- [ ] Task 2.3. Quality smoke: one iteration of existing `assistant-factual` + `assistant-tool-use` fixtures against the best dense and best MoE config (via the manual-endpoint escape hatch, `crates/fono-assistant/src/factory.rs:506-518`). Purpose: integration tripwire (template/leak/grammar breakage), NOT intelligence scoring. Verify `--reasoning off` fixes the `<|channel>thought` leak observed on 12B.
- [ ] Task 2.4. Concurrency check on the winner: `-np 2` with mixed short/long requests; confirm no TTFT collapse for the voice-profile request while a long API request runs (this is the one per-workload interaction that universal stacking cannot paper over).

### Phase 3 — Decision + productization plan (~half a day of writing)

- [ ] Task 3.1. Pick the default model + stack from Phase 2 data; write the results ADR: chosen config, measured numbers, rejected options with reasons, minimum hardware tier mapping (floor stays 4B-class; new tier = winner).
- [ ] Task 3.2. Author the Fono `llama-server` preset INI (router mode: voice model + optional coder/embedding entries, `--sleep-idle-seconds`, universal flags) as the concrete artifact Fono will ship/manage.
- [ ] Task 3.3. Define the integration work for Forge: managed llama-server child process (spawn/supervise/health-check, pinned binary version, model downloads via existing `fono-download`), OpenAI-compat client path already exists. Note interaction with size-budget/packaging (server binary is a separate artifact, not part of the 25 MiB fono binary).
- [ ] Task 3.4. Fold the thinking-channel parsing fix into Fono's client path only if Task 2.3 shows the server-side reasoning flags are insufficient for embedded/legacy paths.
- [ ] Task 3.5. Optional follow-ups worth recording, not blocking: LLGuidance build flag evaluation (faster JSON-schema constraint; needs Rust toolchain in the server build), `/infill` FIM endpoint for editor integration, `/embedding`+`/reranking` for future RAG features, publishing Fono presets as an HF `preset.ini` repo.

## Verification Criteria

- Each Phase 2 run recorded with the exact command line, TTFT, tok/s, acceptance rate, RSS/iGPU numbers, reproducible from `../fono-tmp`.
- The chosen default demonstrates: decode ≥ 10 tok/s and TTFT ≤ 1 s (voice-viable) at quality ≥ the 12B smoke results, on this laptop.
- The universal stack runs unmodified on both a dense and an MoE model (proven by Tasks 2.1 + 2.2 sharing the same base flags).
- Quality smoke shows no output corruption (no leaked thinking markers, valid tool JSON with schema constraint on).
- Licence verdicts recorded for every artifact touched; no ADR-0004 violation in the recommended default.

## Potential Risks and Mitigations

1. **EAGLE-3/DFlash unsupported or broken on Vulkan** (docs example uses `-fa on`; iGPU FA support uncertain)
   Mitigation: Task 1.2 smoke-tests before the full sweep; ngram-mod is the model-agnostic fallback that cannot fail this way.
2. **Draft acceptance collapse on Romanian voice prompts**
   Mitigation: measured separately in 2.1/2.2; speculation is additive — worst case it's disabled for nothing lost, since verification-rejection costs ~nothing.
3. **MoE 26B-A4B doesn't fit the UMA envelope even offloaded** (~14–15 GB Q4 vs ~14 GB iGPU-visible + 30 GB shared, live-Linux with tmpfs pressure)
   Mitigation: `--n-cpu-moe` sweep in 2.2 explicitly finds the fit; mmap keeps cold experts reclaimable; dense-12B+speculation is the fallback winner.
4. **RedHatAI speculator licence incompatible** (Gemma derivative — terms unverified)
   Mitigation: Task 1.1 before download; without it the stack still ships with ngram-mod, EAGLE-3 becomes opt-in.
5. **Server binary distribution** (llama-server is now a shipped artifact Fono must pin/build; interacts with packaging, not the 25 MiB budget)
   Mitigation: Task 3.3 scopes this explicitly; Phase 9 packaging templates already anticipate external deps.

## Alternative Approaches

1. **Embedded-runtime path (llama-cpp-2 + upstream C-API PRs for speculation):** preserves single-binary purity, but re-implements what the server gives free and forfeits router/sleep/slots; revisit only if the managed-child model proves operationally painful.
2. **Per-workload tuned configs instead of the universal stack:** measurably optimal per task, but rejected by design review — management complexity outweighs the marginal gains; the API's per-request overrides (grammar, reasoning budget, sampling) already provide the needed differentiation.
3. **External runtime (Ollama/llama-swap):** least work, but unpinnable dependency and breaks Fono's self-contained install story.
