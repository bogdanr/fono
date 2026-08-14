# Automatic GPU offload sizing — simplified

## Objective

Replace the two contradictory `n_gpu_layers` constants
(`crates/fono-core/src/llama_backend.rs:124-152`: assistant pinned to `0`,
polish at llama.cpp's `-1`) with one automatic rule that is *good enough plus
self-correcting*, rather than precise.

This supersedes `plans/2026-08-12-gpu-offload-auto-sizing-v1.md`. Three of v1's
findings were wrong and one whole stage was unnecessary. The corrections are
below because they are the reason this version is a third of the size.

## What the code said, versus what v1 assumed

**1. The driver already reports the unified-memory budget. v1 proposed
computing it ourselves.**

`ggml_backend_vk_get_device_memory` sums heaps and, when `VK_EXT_memory_budget`
is present, reports `heapBudget[i] - heapUsage[i]` — the driver's own answer to
"how much may you use". For an integrated GPU it deliberately counts *all* heaps,
not just device-local
(`ggml/src/ggml-vulkan/ggml-vulkan.cpp:17383-17414`). Metal reports
`recommendedMaxWorkingSetSize` and a `has_unified_memory` flag
(`ggml/src/ggml-metal/ggml-metal-device.m:734`, `:887`). The backend also
self-classifies via `is_integrated_gpu → GGML_BACKEND_DEVICE_TYPE_IGPU`
(`ggml-vulkan.cpp:17504`), and llama.cpp already prefers a discrete GPU and
takes at most one iGPU (`src/llama.cpp:252-270`).

So your instinct is right: the machine already knows how much unified memory the
GPU may have, and we should ask rather than model. **One narrow gap survives:**
when `VK_EXT_memory_budget` is absent the code falls back to `heap.size`
(`:17410`) — on an iGPU that is all of system RAM. That single fallback is the
entire unified-memory hazard, not the broad problem v1 described.

**2. Offloaded weights are not held twice. v1's double-residency term was
wrong.**

With mmap on, host-side tensors point directly into the mapping
(`src/llama-model-loader.cpp:1382`), device-side tensors are copied in
(`:1532`), and at the end of loading `unmap_fragment` trims the mapping to the
span the CPU still uses (`:1666-1673`) — the code's own comment is "unmap
offloaded tensors and metadata". Task 14 of v1, and the risk built on it, are
deleted.

**3. Upstream's fitter is excellent and we cannot afford it.**

`common/fit.cpp` does not estimate — it trial-loads the model and context, reads
the per-device memory breakdown, and adjusts
(`common_get_device_memory_data_impl`, `common/fit.cpp:99-150`). Far better than
v1's Tasks 10-17. But it lives in `common/`, and our own `Cargo.toml:113-123`
disables that feature on purpose to strip "`libcommon.a` ~14 MB + wrapper
archive ~10 MB", against a 23.10 MiB binary and a 25 MiB gate. The empirical
route is closed too: `llama_memory_breakdown` and `llama_model_n_devices` are not
in the bindgen-visible headers (`wrapper.h` includes only `llama.h`, `gguf.h`,
`wrapper_common.h`).

Its header also states the assumption that matters: *"fits mparams and cparams
to free device memory (**assumes system memory is unlimited**)"*
(`common/fit.h:14`). On a unified machine free device memory *is* system memory,
so even upstream's fitter would need our one extra cap.

## Your first question: streaming and offload compose

They are not alternatives. Partial offload puts N layers in device memory and
leaves the rest mmapped and demand-paged from disk — that is the same code path,
not a special mode. So on a model larger than memory you can offload whatever
fits and stream the remainder.

**Superseded in practice by the offload curve** (see Stage 2). The mechanism is
real, but a partial offload is not a partial win: decode at half the layers is
*slower* than no offload at all. So a model that does not fit whole is left
entirely on the host and streamed, and the composition below is of interest only
if a future measurement finds a device where partial decode does scale.

The framing that makes it obvious: on an over-subscribed model, RAM is a cache
over the model file. Converting a gigabyte of *evictable page cache* into a
gigabyte of *pinned device weights* is strictly better — guaranteed resident and
faster to compute.

- **Discrete card:** compounding win. Device memory is separate, so offloading
  also frees system RAM, enlarging the page cache for the streamed remainder.
- **Unified:** a straight trade of page cache for pinned weights. Still
  positive, but it does not compound, and it is the case where the guard matters.

One honest limit: DeepSeek-V4-Flash is 104 GB against 30 GB. Offloading ~8 GB
accelerates ~8 % of layers. 269 ms/token prefill will not become usable. The
mechanism composes; it does not rescue that model.

## Your third question: yes, and here is what dissolves

I was designing for **precision** when the right target is **convergence**. A
retry ladder plus a persisted answer makes an imprecise first estimate
harmless — the machine converges on its own answer once, and never pays again.
That single change deletes per-tensor costing, the KV term, the compute-buffer
term, the margin taxonomy, and the class-differentiated accounting.

**Superseded by the offload curve** (see Stage 2). The measurement removed the
thing precision was for: there is no layer *count* to converge on, because a
partial offload is not a partial win. Prefill scales with the fraction moved, but
generation does not — at half the layers it is slower than not offloading at all,
since each decode call submits one token and pays the device boundary crossing in
full. The ladder and the persisted answer are both cut — the ladder because it
searches for a count that does not exist, persistence because a remembered answer
goes stale the moment the browser takes half the machine.

The whole strategy is now a single yes/no, computed fresh at every load:

- **Ask the driver** how much it may use (`ggml_backend_dev_memory`).
- **Cap once for unified memory**: additionally bound by `available_ram_bytes`
  from the existing hardware probe, minus a desktop reserve. This is the only
  class-specific line, and it covers the missing `VK_EXT_memory_budget` fallback.
- **All or nothing.** If the whole model plus its KV and compute buffers fit
  under that budget, offload every layer. Otherwise stay on the CPU.

A failed load falls back to CPU-only for that load. Nothing is remembered.

No new dependency. The `ggml_backend_dev_*` symbols are in the already-linked
static ggml but absent from `wrapper.h`, so they get a hand-written `extern "C"`
block — exactly the precedent already set by the hand-rolled `statvfs` in
`crates/fono-core/src/hwcheck.rs:570-593` and by `fono-core::brain_tap` reaching
raw `llama-cpp-sys-2` bindings (`Cargo.toml:124-129`).

## Implementation Plan

### Stage 0 — Two gates (the only questions that can invalidate the design)

- [x] Task 1. **Establish whether a device allocation failure is catchable or
  fatal.** Force an over-commit and observe whether it surfaces as a Rust error,
  a `GGML_ABORT`, or a driver crash. Rationale: this decides whether Fono may
  attempt an offload it believes will fit. Catchable → attempt it and fall back
  to the CPU if wrong. Fatal → never attempt anything not provably safe, because
  a wrong estimate would crash-loop at startup.

  **Result: catchable, at both allocation sites. Fono may attempt the offload
  and recover.** Measured on an Arc 140V, Vulkan build.
  - *Weights over-commit* — a 25.6 GB Q8_0 model at all layers against ~20 GB
    reported free: `ggml_vulkan: vk::Device::allocateMemory:
    ErrorOutOfDeviceMemory` → `unable to allocate Vulkan0 buffer` → `failed to
    load model` → a clean Rust `Err` out of the loader. No abort, no driver
    reset, process survived and exited normally.
  - *KV over-commit* — a 262,144-token context: `failed to allocate buffer for
    kv cache` → `failed to initialize the context` → a clean Rust `Err` out of
    context creation.

  Second finding, worth as much as the first: **llama.cpp does not clamp the
  request to what fits.** It attempts the allocation and fails. An earlier
  observation that it self-clamped to zero was an artefact of the measurement
  seam — `with_n_gpu_layers` takes `u32`, so the wrapper cannot express
  llama.cpp's `-1` sentinel at all, and the unwired seam had left the value at
  `0`. This is what makes the CPU fallback necessary rather than merely prudent:
  nothing below us will trim an over-ambitious request.

- [x] Task 2. **Establish whether a saved KV state stays valid across an
  `n_gpu_layers` change.** Save with layers on the CPU, restore with layers on
  the device, compare bytes and output. Rationale: if it does not, the offload
  decision must join `swa_full` and the KV types in the cache runtime identity.
  This is the silent-wrong-restore class that already cost a day of this work,
  so it is tested, not assumed.

  **Result: the state is portable. The offload decision must NOT join the cache
  runtime identity.** Three arms on `gemma-4-e2b`, 376-token prefix, save and
  restore differing in offload:

  | Arm | Offload at save → restore | State bytes | Round-trip diff | Reply |
  |---|---|---|---|---|
  | cpu-cpu | 0/36 → 0/36 | 3,691,218 | 0 | `42` |
  | **cpu-gpu** | **0/36 → 36/36** | **3,691,218** | **0** | **`42`** |
  | gpu-gpu | 36/36 → 36/36 | 3,691,218 | 0 | `42` |

  The mixed arm loaded two separate copies of the GGUF (confirmed in the loader
  log: `offloaded 0/36` then `offloaded 36/36` — `ModelKey` includes
  `n_gpu_layers`, so `shared_model` cannot alias them). Serialized size is
  byte-identical and re-saving from the restored context reproduces the original
  exactly, so the KV serialization format is device-independent. A cache written
  before offload was enabled stays usable after, and vice versa — one fewer
  invalidation, and one fewer way to be silently wrong.

  An incidental timing taken here (prefill 60 ms against 5,740 ms) is
  **retracted**: the CPU arm ran with a cold page cache immediately after a
  25 GB model load, so most of it was disk I/O. Warm and repeated, the same
  comparison is 1.5× (Stage 2).

### Stage 1 — Ask the machine, and show the answer before acting on it

- [x] Task 3. Hand-declare the `ggml_backend_dev_count` / `_get` / `_memory` /
  `_type` externs in `fono-core`, following the existing `statvfs` precedent.
  Rationale: the symbols are already linked; this reaches them without touching
  bindgen, without a patched crate, and at zero size cost.
  **Done** — `crates/fono-core/src/ggml_devices.rs`, gated on `llama-local`.
  `ggml_backend_dev_description` was added to the four (the marketing name is
  what makes the doctor line legible). `ggml_backend_dev_get_props` was
  deliberately *not* bound: it returns more in one call but would commit the
  file to the layout of a C struct upstream is free to extend. Size budget
  unchanged at 23.10 MiB, confirming the zero-cost claim.
- [x] Task 4. Build a device inventory: name, type, free and total memory, and
  whether the reported figure came from a real budget or the whole-heap
  fallback. Rationale: the fallback flag is what triggers the one unified guard,
  so it must be captured rather than inferred.
  **Done.** The fallback is detected exactly rather than by threshold:
  `ggml-vulkan.cpp:17383-17414` accumulates `total` and `free` over the same
  heaps, differing only in the term added to `free` — the driver's budget when
  `VK_EXT_memory_budget` is present, the whole heap size when it is not. So the
  sums are equal exactly when the query is unavailable. A genuinely idle device
  can trip the same equality; that direction only wastes capacity, never
  over-commits. An unrecognised `ggml_backend_dev_type` discriminant is
  preserved as `Unknown(n)` and treated as not having memory of its own.
- [x] Task 5. Reconcile with ADR 0028's host-GPU classification and
  `crates/fono-core/src/vulkan_probe.rs` rather than adding a second taxonomy.
  Rationale: two classifications of the same hardware will disagree eventually,
  and the disagreement will be invisible.
  **Done, as a documented boundary rather than a merge** — they answer different
  questions and neither can replace the other. `HostGpu` answers *how fast*, from
  a Vulkan-only subprocess probe that works in builds without llama.cpp and
  before any model exists; it carries no memory figure at all. `ggml_devices`
  answers *how much fits*, from the registry llama.cpp will pick from at load
  time. Merging them would force the capacity question to be answerable in
  builds that cannot link ggml. Recorded in ADR 0028's deliberate non-decisions
  as an explicit split of authority: `HostGpu` decides speed, ggml's registry
  decides capacity, neither derived from the other. On this laptop both agree
  (`IntegratedTensor` / `IGpu`) — so nothing forces the issue today, which is
  exactly why the boundary is worth writing down before something does.
- [x] Task 6. Report the inventory in `fono doctor` **before** any behaviour
  change. Rationale: it makes the probe reviewable on machines we cannot test —
  including 10.10.0.136 — at zero risk, and turns "go and collect snapshots"
  into something any user can run.
  **Done.** Measured on this laptop:

  | Build | Reported |
  |---|---|
  | CPU | `Intel(R) Core(TM) Ultra 7 258V — CPU (30 GB, shared with system memory)` |
  | Vulkan | `Intel(R) Graphics (LNL) — Vulkan0 (23 GB, shared with system memory)` plus the CPU row |

  Raw figures behind the Vulkan row: `free=22346203136`, `total=24829612032`,
  `kind=IGpu`. Three things this settles for Stage 2. The backend
  self-classifies as `IGPU`, so the unified case needs no heuristic of ours.
  `free < total`, so `VK_EXT_memory_budget` *is* present on this driver and the
  whole-heap fallback is not the common path. And the device reports 23 GB of
  "free" memory on a machine with 30 GB of RAM total — which is precisely the
  over-commit Task 8's cap exists to stop, observed rather than hypothesised.

### Stage 2 — One fit test, one guard, computed fresh every load

The offload curve (`docs/bench/prompt-cache-2026-08-11.md`) reshaped this stage.
Two findings drive it. First, the gain on an *integrated* device is **1.3×
prefill and 2.1× decode**, not the 4-6× an earlier single-run pair claimed — and
buying it costs ~10 GB of *pinned* memory out of the same pool the desktop uses.
Second, re-run with three interleaved repeats per point, the partial-offload
curve splits: prefill scales with the fraction moved, but decode does not, and at
half the layers decode is **slower than not offloading at all**. Each decode call
submits one token, so the boundary crossing between devices is paid in full on
every token produced; prefill submits 512 per call and amortises it away.

Together those say the decision is **binary**. There is no polite middle setting:
a partial offload chosen to spare the desktop makes generation worse, and
generation is what the local dictation path is made of. Offload every layer or
none, and let the memory budget decide which.

- [x] Task 7. Decide *whether* the whole model fits: total model bytes (already
  available from the shard-aware fingerprint enumeration) plus the KV and compute
  buffers for the configured context, against the budget from Task 8. Rationale:
  the curve leaves only two settings worth choosing between, so this is a fit
  test rather than a sizing calculation, and a reader can verify it by inspection.

  **Done**, in `crates/fono-core/src/gpu_offload.rs`. Weights are the sum of the
  shard files, working memory a flat 1 GiB (measured 523 MiB of compute buffers
  against 9,141 MiB of weights), and the cache is costed per block from the
  GGUF's own metadata table.

  Two things the metadata forced, both found by cross-checking the estimate
  against the per-token figures already measured:
  - **A `vocab_only` model load cannot be used to ask these questions.**
    llama.cpp returns from its hyperparameter reader *before* the attention keys
    when `vocab_only` is set, leaving the layer count at zero, and the head-count
    accessor then calls `GGML_ABORT` — the process dies inside a sizing
    decision. The key-value table is read directly instead, which is cheaper and
    cannot abort.
  - **Blocks are not interchangeable, and averaging them is wrong by multiples.**
    Head counts and head widths are per-layer arrays. gemma-4-26B keeps eight
    256-wide heads on its sliding-window blocks and two 512-wide on its five
    full-attention ones; Qwen3.6-35B is a hybrid where three blocks in four are
    recurrent and cost nothing per token, their fixed ~64 MiB state absorbed by
    the working-memory allowance.

  Verified against the measured per-token costs: gemma-4-26B 225,280 B at `f16`
  (measured 220 KB) and 119,680 B at `q8_0` (measured 116.9 KB); Qwen3.6-35B
  20,480 B at `f16` (measured marginal ~20 KB). Both models then loaded whole
  onto the iGPU through the real role path; the 97 GB DeepSeek-V4-Flash was
  refused and ran on the host.
- [x] Task 8. Budget by device class, which `ggml_backend_dev_type` already
  reports, so this needs no heuristic of ours:
  - **Discrete** — reported free device memory less a flat margin. The memory is
    a separate pool, so taking it costs the desktop nothing.
  - **Integrated, or whenever the whole-heap fallback was used** — additionally
    bound by `available_ram_bytes` from `hwcheck` minus a desktop reserve.

  Rationale: on an integrated device the "free device memory" figure is system
  RAM (23 GB reported on a 30 GB machine, measured in Task 6), and every
  offloaded byte converts *evictable page cache* into *pinned* memory the kernel
  cannot reclaim while the model is loaded. That is the failure a step-down
  ladder cannot see, because the allocation succeeds — the machine simply gets
  slower for everything else. It is also the cap upstream's own fitter would
  need, by its own stated assumption that system memory is unlimited.

  **Done.** A discrete device reporting a real budget is believed as-is;
  anything else — integrated, or a driver that answered with its whole heap —
  is additionally bound by `available_ram_bytes` less a 4 GiB desktop reserve.
  A platform that cannot report free memory therefore offers no budget and the
  model stays on the host.
- [x] Task 9. On a failed load, fall back to CPU-only for that load and log it.
  No search, no retry sequence. Rationale: Task 1 established the failure is a
  clean `Err`, so one fallback is enough to make a wrong fit test slow rather
  than fatal. A bounded ladder would spend several multi-second load attempts
  hunting intermediate counts the curve shows are worse than either endpoint.

  **Done**, with a warning that says the reply will be slower but correct.
- [ ] ~~Task 10. Persist the working answer keyed by model fingerprint and
  device identity.~~ **Cut.** The answer is only valid for the moment it was
  measured: it worked yesterday when 20 GB was free, and today the browser has
  half the machine, so the remembered number is now wrong in the dangerous
  direction. Recomputing costs a `ggml_backend_dev_memory` call and one
  division — cheaper than the staleness it would introduce, and it deletes the
  tmpfs-tolerance and back-off bookkeeping with it.
- [x] Task 11. Route both roles through this one call site, with per-role inputs.
  Rationale: this is the instruction — one mechanism, no per-role constants —
  while still giving the right answer if the roles are configured with different
  models. A shared *constant* would be a regression dressed as consistency.

  **Done.** `shared_model_sized` replaces both per-role constants, and the
  cleanup role's `LlamaModelParams::default()` — whose `-1` asked for everything
  and failed the load outright on a card too small — is gone with them.
- [x] Task 12. Verify offload does not change answers: the graded coding suite
  scoring the same with and without. Rationale: reduction order differs between
  backends, so byte-equality is the wrong gate. Using it is exactly the mistake
  that produced a retracted finding earlier in this work.

  **Done — identical, and stronger than the gate asked for.** Both arms score
  **7/10** on all three interleaved repeats, and the agreement is per task: the
  same three tasks fail, with the *same* failure string, in all six runs. Nothing
  moved between arms except which chip held the weights.

  | Arm | Repeats | Pass | Wall | TTFT median |
  |---|---|---|---|---|
  | CPU (`fono`, default features) | 3 | 7/10, 7/10, 7/10 | 230.7 / 231.8 / 230.7 s | 3.71 s |
  | Device (`--features accel-vulkan`) | 3 | 7/10, 7/10, 7/10 | 184.2 / 162.3 / 167.1 s | 1.31–1.95 s |

  Method: two `release-slim` binaries differing only in `accel-vulkan`, one
  isolated XDG profile serving gemma-4-26B-asym as the assistant over
  `[server.llm]` at `ctx = 4096`, graded by `fono-benchmark/capability.py
  --endpoint` (temperature 0, seed 42, ten tasks: eight Python, one Rust, one
  C++). Arms alternated cpu/gpu/cpu/gpu/cpu/gpu, one model load each. The device
  arm logged `model needs 10.4 GB (8.9 weights + 0.5 cache + 1.0 working), 16–21 GB
  available — running on the device`; the CPU arm logged `no accelerator
  registered`.

  The three standing failures are the model and the harness, not offload:
  `roman_to_int` emits a Python syntax error, and both compiled tasks leak a
  `<|channel>thought` preamble that lands a stray backtick in the source. They
  fail identically without an accelerator, so they are outside this task.

  Wall time is a by-product here, not the measurement — the suite runs unniced but
  the grader compiles between calls, so read the offload curve above for speed.

### Stage 3 — UX

- [x] Task 13. Report in plain language in `fono doctor` and the diagnostics
  panel — which device, how much of the model is on it, what stopped more going
  on — and fail invisibly: a refused or failed offload is a debug log and a
  normal, slower turn. No toast, no startup warning. Rationale: a wrong
  automatic answer must be diagnosable by the person hitting it without them
  learning what a layer is; acceleration should only be noticed by things being
  fast.

  **Done.** `fono doctor` prints one line per role that loads a local model,
  naming the device and the arithmetic behind the answer — the same words the
  daemon logs, e.g. `assistant offload: Intel(R) Graphics (LNL): model needs
  10.4 GB (8.9 GB weights + 0.5 GB cache + 1.0 GB working), 20.8 GB available —
  running on the device`. It is a fresh decision, not a record of one: free
  memory moves, so a diagnostic can only say what would happen now. Nothing was
  added to the failure path — a refused offload stays a debug log and a slower
  turn.
- [x] Task 14. Assert zero new configuration keys via a serialized
  default-config diff before and after. Rationale: makes the no-knobs constraint
  a gate rather than an intention.

  **Done.** `the_decision_is_not_configurable` serialises the default config and
  fails on any key naming a device, a layer count or an offload switch, so the
  no-knobs rule now breaks the build rather than a promise.

### Stage 4 — Measure, then re-sequence the cache work

- [~] Task 15. Measure on the unified laptop, on 10.10.0.136, and on a discrete
  card: prefill and decode per token, restore time, peak RSS, and whether the
  model was offloaded and why — plus a deliberate over-commit on the discrete
  card to prove the CPU fallback fires. Record in `docs/bench/` with model,
  quantization and host, at normal priority with at least three repeats per arm.
  Rationale: the existing numbers are labelled CPU-only and these supersede
  them; a discrete card is the only place the all-or-nothing rule can be judged
  where the memory it pins costs the desktop nothing. Repeats are mandatory —
  single runs produced two retracted findings in this work.

  **Both unified hosts measured; the discrete card is still missing.**
  10.10.0.136 is a second *unified* machine (Ryzen AI MAX+ 395 / Radeon 8060S),
  so it cannot settle the all-or-nothing rule either — that question stays open
  until a discrete card is available. What it did settle is worth more than a
  timing, and it overturned an earlier reading of the same machine. **The box
  has 128 GB installed; its firmware hands 96 GiB to the GPU before Linux
  boots**, so Linux reports 31 GB and RADV's 111 GB is the honest sum of a
  96 GiB private heap and a 15.6 GiB shared aperture. Bounding the device by
  system RAM offered a 19 GB budget and sent every model between 19 GB and
  96 GiB to the CPU. Counting the carve-out — anything the device reports
  beyond all the RAM the kernel knows about — raised the budget to 99.7 GB and a
  97 GiB four-shard DeepSeek then loaded with 95.7 GiB resident on the device.
  That model separately answers *wrongly* on the device. Two attributions were
  written and both were wrong — first Vulkan's missing Lightning Indexer and
  fused HC ops (that warning disables the fused path and falls back, costing
  speed, not correctness), then llama.cpp#25436. Upstream `llama-cli` b10405 on
  the same machine and shards, all layers on the device, answers correctly under
  every one of Fono's settings, so the fault is ours and nothing goes upstream.
  It is not a sizing fault; both findings
  are in `docs/bench/prompt-cache-2026-08-11.md` along with the fits case
  (qwen3.5-2b → device, ~2.9× the wall speed of the CPU arm over 128 tokens,
  three repeats per arm, arms alternated twice).
- [ ] Task 16. Re-derive the disk cache tier's value against post-offload costs.
  Rationale: measured on the integrated laptop, offload cuts prefill only
  12.81 → 9.93 ms/token (1.29×) while *doubling* restore on the 26B model
  (130 → 269 ms).
  The cache's value is prefill avoided minus restore paid, so offload narrows it
  from both ends — much less than the retracted 4-6× figure suggested, but in
  the same direction. Sizing the tier first would size it against numbers that
  are about to move.

## Verification Criteria

- The serialized default configuration is byte-identical before and after.
- One call site computes `n_gpu_layers`; no per-role constant remains in
  `crates/fono-core/src/llama_backend.rs`.
- No new dependency; `./tests/check.sh --size-budget` passes with no measurable
  growth, and `common` remains disabled.
- On the unified laptop, repeated launches plus a full-context turn leave the
  desktop responsive with no OOM kill.
- On a discrete card, a deliberate over-commit falls back to a CPU-only load and
  logs it, with no crash and no retry sequence.
- Prefill and decode per token improve measurably against the recorded CPU
  baseline, at normal priority, with at least three repeats per arm. On the
  integrated laptop the measured gains are 1.29× prefill and 2.06× decode; a
  threshold higher than that would fail on the only machine we have.
- The model is either fully offloaded or not offloaded at all; no run reports a
  partial layer count.
- The graded coding suite scores the same with and without offload.
- A saved KV state either restores correctly across an offload change, or the
  decision is in the runtime identity and old states miss cleanly.
- `fono doctor` names the device and says whether the model is on it, on all
  three machines.

## Potential Risks and Mitigations

1. **Unified memory over-commits and starves the desktop.** Not an OOM kill —
   Task 1 disproved that — but the worse failure, because the allocation
   *succeeds*: offloaded weights are pinned and unreclaimable, where host-side
   weights are evictable page cache. Nothing errors, nothing logs, the machine
   simply gets slower for everything else, and no retry mechanism can see it.
   Mitigation: Task 8's cap against `available_ram_bytes`, triggered on `IGPU`
   or on the whole-heap fallback, which is the precise condition where the
   driver's figure is untrustworthy. This is why the cap must exist
   independently of any failure-handling.
2. ~~**Allocation failure aborts rather than returning**, turning a retry into
   a crash loop.~~ **Settled by Task 1: it returns cleanly at both the weight
   and the KV allocation sites.** That is what makes Task 9's single CPU
   fallback sufficient — an over-optimistic fit test costs one slow load, not a
   failed start.
3. ~~**A saved KV state is silently invalid across an offload change.**~~
   **Settled by Task 2: the state is device-independent.** The offload decision
   stays out of the cache runtime identity.
7. **Polish already asks for full offload and can fail the load outright.**
   `LlamaModelParams::default()` leaves llama.cpp's `-1` in place for polish
   (`crates/fono-core/src/llama_backend.rs:124-143`), and Task 1 established
   that this does not clamp to what fits — it attempts and errors. On a machine
   with a small card and a larger polish model, polish model loading fails today
   rather than degrading to the CPU. Uncovered by Gate 1 rather than sought.
   Mitigation: the single policy replaces `-1` for both roles, so this
   disappears with the rest of the split; until then it is a latent failure on
   `accel-*` builds, not a regression this work introduces.
4. **The coarse estimate is badly wrong on mixture-of-experts models**, where
   per-layer size is uneven.
   Mitigation: accepted by design, and narrowed by all-or-nothing — the estimate
   no longer picks a count, only answers whether the whole model fits, so an
   uneven per-layer size cannot produce a bad partial split. The margin absorbs
   the error, and a failed load falls back to CPU. This is the trade that buys
   the simplicity.
5. **Reported free memory is advisory and some drivers lie.**
   Mitigation: the margin, and the CPU fallback on a failed load. A lying driver
   costs one wasted load attempt per launch rather than a failure.
6. **Offload makes cached turns worse on larger models** — restore doubled on
   the 26B model (130 → 269 ms) because a restored state is uploaded to the
   device, while prefill only improved 1.29×.
   Mitigation: this is now a genuinely close call rather than the easy win the
   retracted 4-6× figure implied, and it is the reason Task 16 re-derives the
   disk tier's value rather than assuming it.

## Alternative Approaches

1. **Enable `common` and call `common_fit_params`.** The best algorithm, already
   maintained, MoE-aware. Rejected on our own recorded decision: `common` was
   disabled to strip ~24 MB of archives against a 25 MiB gate. Worth
   reconsidering only if the GPU build artifacts get their own, looser budget —
   and even then it needs Task 8's cap, since its header states it assumes
   system memory is unlimited.
2. **Leave `-1` for both roles and rely only on retrying downward.** Simpler
   still: no estimate at all. Task 1 found failure *is* cleanly catchable, which
   was the condition that would have made this attractive — but it is rejected on
   two firmer grounds. Mechanically, `with_n_gpu_layers` takes `u32`, so the
   wrapper cannot express `-1` at all; reaching it needs an FFI escape. And on
   unified memory `-1` means "offload everything into system RAM", which
   *succeeds* and takes the desktop's memory — the one failure retrying cannot
   detect.
3. **All-or-nothing: offload the whole model or none of it.** **Adopted** — see
   Stage 2. Considered and rejected earlier on the argument that a partial
   offload is how Fono leaves the desktop its memory. The re-run curve killed
   that argument: a partial offload is not a partial win. Prefill scales with the
   fraction moved, but decode does not, and at half the layers decode is slower
   than not offloading at all — so a partial offload chosen to be polite about
   memory makes the local dictation path worse. The layer count is worthless as a
   speed dial *and* unusable as a budget dial, so it goes entirely.
4. ~~**Conservative-first, learn upward.**~~ **Discharged: Task 1 found failure
   catchable, so this is not mandatory.** Kept only as the fallback shape if a
   driver is ever found that aborts instead of returning.
5. **Ship a per-class table of defaults and no probe.** Cheapest. Rejected: a
   knob wearing a table's clothing, blind to model and context size, and wrong
   hardest on the unified class that is two of our three machines.
