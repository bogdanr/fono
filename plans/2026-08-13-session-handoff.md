# Session handoff — 2026-08-13

Scratch note for resuming after a context reset. Delete once the work below lands.

## Git state

- Branch `main`, **1 commit ahead of `origin/main`, unpushed**: everything
  since 0.18.1 squashed into one — long-prompt chunked prefill, multi-shard fingerprint,
  API tool support, `q8_0` KV, `fono doctor` compute listing, automatic GPU offload and
  its verification, the Parakeet decision, and the bench docs.
- Safety branches `backup-presquash-1786574710` and `backup-presquash-task12` hold the
  pre-squash histories. `git diff backup-presquash-task12 HEAD` is empty, so the squash
  changed no content. Delete both when satisfied.
- Why the second squash: an `--amend` landed on the wrong commit mid-session, so the
  GPU-offload implementation ended up wearing the Parakeet subject and body. The squash
  fixed the mislabelling.

## Active plan

`plans/2026-08-12-gpu-offload-auto-sizing-v2.md`.

- **Stage 0 (gates) — done.** An over-commit returns a clean Rust `Err`, not an abort,
  but llama.cpp does **not** clamp the request to what fits, so the caller must pick a
  number that fits. A saved KV state is device-independent, so the offload decision
  stays **out** of the cache runtime identity.
- **Stage 1 (report first) — done.** `crates/fono-core/src/ggml_devices.rs` reads ggml's
  device registry through six hand-declared `extern "C"` entry points. `fono doctor`
  prints a `compute` row per device, and the web settings doctor picks it up for free
  because `renderDoctor` maps over `doctor.sections` generically.
- **Stage 2 — done.** `crates/fono-core/src/gpu_offload.rs` decides all-or-nothing per
  load; `llama_backend::shared_model_sized` is the single call site both roles use. Size
  budget 23.12 MiB. Task 12 confirmed offload does not change answers: 7/10 on the graded
  coding suite on both arms across three interleaved repeats, with the same three tasks
  failing the same way in all six runs (see the plan and
  `docs/bench/prompt-cache-2026-08-11.md`).
- **Stage 3 (UX) — not started.**

## Notes on Stage 2

Tasks 7-9, 11 and 12 carry their results inline in the plan; the short version is that
the per-token cache estimate now matches every
measured figure (gemma-4-26B 225,280 B at `f16` against 220 KB measured, 119,680 at
`q8_0` against 116.9 KB; Qwen3.6-35B 20,480 B at `f16` against a ~20 KB marginal), and
two traps are documented there: a `vocab_only` load aborts the process rather than
answering, and per-layer head geometry cannot be averaged — gemma varies head width 2×
between block kinds and Qwen3.6 is a hybrid whose recurrent blocks cost nothing per
token.

## Backlog after Stage 2

1. **Push.** One unreleased commit, carrying three real defect fixes.
2. **The disk tier** — the user's original request, still unbuilt. Size it against
   20-220 KB/token, no per-entry ceiling, and add a refusal when the cache directory is
   memory-backed (this machine would otherwise eat RAM while believing it used disk).
3. **`/api/show` does not exist** and `/api/tags` reports `size: 0` with an empty digest;
   several Ollama clients treat that as unusable, which blocks tool-capable clients from
   trying the endpoint we just fixed. Cheap.
4. Deferred: sub-8-bit KV (needs a model whose cold arm is stable — the 2-bit test model
   cannot resolve it), streaming tool calls (saves seconds against prefill's minutes).

## Measurements that drive the design

Machine: Intel Core Ultra 7 258V, 8 cores, 30 GB RAM, Arc 140V integrated (`IGPU`,
Vulkan), NVMe. `/root` is **tmpfs** — `~/.cache/fono` was symlinked to
`/mnt/150g/fono-cache` as a local workaround so cache writes hit the SSD, not RAM.

All numbers in `docs/bench/prompt-cache-2026-08-11.md`.

### Offload curve (gemma-4-26B-asym, 30 repeating layers + output, 3 interleaved repeats per point, warm page cache, medians)

| Layers on card | Prefill ms/tok | Decode tok/s |
|---|---|---|
| 0 | 12.81 | 8.53 |
| 8 | 11.84 | 9.36 |
| 15 | 11.66 | **6.07** |
| 23 | 10.33 | 7.83 |
| 31 (all) | **9.93** | **17.57** |

Prefill scales roughly linearly with the fraction moved. Decode does **not**: full
offload is 2.06×, every partial setting is worthless, and 15 layers is *slower than not
offloading at all*. Mechanism is batch size — prefill submits 512 tokens per call so the
device boundary crossing amortises away; generation submits one token per call and pays
it every token. Generation is what the dictation path is made of, so a partial offload
chosen to be polite about memory would make the felt part slower. Hence all-or-nothing.

Full offload here pinned ~10 GB (9,141 MiB weights + 467 KV + 523 compute).

**No discrete card has been measured. These numbers must not be used to size one.**

### KV cost

`type_k = q8_0`, `type_v = q8_0` shipped (assistant only; both folded into the cache
runtime identity so stale checkpoints miss cleanly). 220.0 → 116.9 KB/token on
gemma-4-26B, **46.9% off**, restore 609 → 303 ms. Nothing is converted on save or
restore — the tensor type is recorded and rows are copied — so fewer bytes is simply
faster.

Per-token KV cost varies ~8× across models of similar size because it is
`n_layer × n_head_kv × head_dim × 2 × bytes` and every factor differs; on a hybrid the
layer count is the *attention* layers only, a quarter of the total on Qwen3.6. Model
choice dominates any cache policy.

### Retractions (mine, all from single-run measurements)

- "GPU prefill is 4–6× cheaper" — one run per arm while the machine was downloading
  104 GB and compiling. Real figure is **1.29×** end-to-end prefill.
- "5,740 → 60 ms, 95×" — the CPU arm ran with a cold page cache right after a 25 GB
  model load evicted everything. Most of it was disk I/O.
- "Saved states above 1 GiB restore corrupt" — the 1 GiB admission ceiling was shipped
  and then **removed**. The fixture was at fault: 9,428 tokens of notes plus a one-line
  question on a 2-bit quant is a coin flip between answering and continuing the
  document, and three of four arms continued — including a cold one. Round-trip diff at
  1.978 GiB is 0 bytes. The strict short-restore check was kept.

**Process rule earned the hard way: three repeats per arm, interleaved, page cache
warmed identically. A single pair of runs cannot resolve a 1.5× difference.**

## Constraints to keep in mind

- Every `cargo` / `tests/check.sh` invocation under `nice -n 10`; **never** nice a timed
  measurement.
- Pre-commit gate, in order: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`. Plus
  `./tests/check.sh --size-budget` before any push.
- No new config keys without justification; the user has rejected knobs repeatedly.
- Commits signed off, user-friendly, no agent co-author trailer.
- Do not push unless told to.

## Verifying the offload sizing by hand

The unit tests prove the arithmetic; only a real GGUF and a real driver show whether the
*inputs* are right. `gpu_offload::tests::sizes_a_real_model_on_this_machine` prints the
per-block geometry, the per-token cache cost at both types, the decision, and then
actually loads the model the way a role does:

```
FONO_TEST_GGUF=/path/to/model.gguf nice -n 10 cargo test \
  -p fono-core -p fono-polish --features fono-polish/accel-vulkan \
  --lib gpu_offload -- --ignored --nocapture
```

Cross-check the printed per-token figure against the measured one for the same model
before believing a change to the estimator — a wrong head count still produces
plausible-looking totals.

## Verifying offload does not change answers

Build two `release-slim` binaries differing only in `accel-vulkan`, point an isolated XDG
profile at a model that fits the card, serve it as the assistant over `[server.llm]`, and
grade both with `fono-benchmark/capability.py --endpoint`. The scratch profile and the
six result files live under `/mnt/150g/task12/` (`run-arm.sh <cpu|gpu> <rep>` reruns one
arm end to end). Compare *scores*, not bytes.
