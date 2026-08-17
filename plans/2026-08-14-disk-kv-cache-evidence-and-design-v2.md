# Disk KV Cache: Prove What Works, Then Build It

*v2 — supersedes v1. Adds the model selection, resolved from the existing
capability scores, and the asym-vs-Q8_0 arm you asked for. The proposal
assessment in v1 is unchanged and restated here in short form; v1 has the long
reasoning.*

## Objective

Decide from measurement what the disk-backed KV cache must be, against two
goals:

1. **Fast restore across a Fono restart.**
2. **Make a local coding agent feasible on machines that cannot otherwise run
   one** — target working context **16k tokens**.

Stage A's deliverable is a **go/no-go with numbers**, not code.

---

## Model selection — resolved

### What the existing scores say

`fono-benchmark/capability.py` has already scored both candidates. Current
results directory (the archived `_cache-backup-2026-07-22/` set was produced by
a noisier harness and must not be mixed in — every model that appears in both
scores the same or worse there):

| Family | Quant | Pass | Total wall | Median task wall |
|---|---|---|---|---|
| gemma-4-26B-A4B | Q8_0 | **10/10** | 553 s | 43.5 s |
| gemma-4-26B-A4B | q4_0 | **10/10** | 245 s | 14.8 s |
| gemma-4-26B-A4B | IQ2_XXS | **10/10** | 279 s | 16.6 s |
| gemma-4-26B-A4B | asym v1 (`gu.iq2xxs-dn.q4_0`) | **10/10** | 288 s | 18.4 s |
| gemma-4-26B-A4B | asym v15 (`gu.iq2xxs-dn.q2k-pad`) | **10/10** | 292 s | 18.6 s |
| qwen3.6-35B-A3B | Q8_0 | **10/10** | 670 s | 60.1 s |
| qwen3.6-35B-A3B | IQ4_XS | **10/10** | 247 s | 18.6 s |
| qwen3.6-35B-A3B | IQ2_M | 9/10 | 244 s | 16.9 s |
| qwen3.6-35B-A3B | asym (`guiq2xxs-dnq2k`) | 9/10 | 241 s | 20.2 s |

Gemma sweeps every tier; Qwen drops one task at both low-bit tiers. **At Q8_0
the two are tied.** The entire gap is the single hard Rust task
(`rust_expr_eval`), which Qwen's low-bit builds fail on a runtime panic and a
hallucinated crate respectively.

**Do not treat that gap as significant.** A ten-task suite has 0.1 granularity,
so 1.00 vs 0.95 is one task wide, single-seeded.

### Why capability is the wrong tiebreak here

The quantity this entire feature spends is **KV bytes per token**, and the two
candidates are already measured about **11× apart**:

| Model | KV/token, f16 | KV/token, q8_0 | 16k checkpoint, q8_0 |
|---|---|---|---|
| gemma-4-26B-A4B | ~225 KB | ~117 KB | **≈ 1.9 GB** |
| qwen3.6-35B-A3B | ~20 KB | ~10 KB | **≈ 165 MB** |

Gemma's hybrid attention layout costs an order of magnitude more per token than
Qwen's. At 16k that is the difference between a checkpoint a laptop can hold and
one it cannot — and it changes blob size, save time, restore time, write
amplification and disk budget all at once.

**So the right decision is not to pick a winner. It is to run both, because
they bracket the design space.** Two points 11× apart give the sensitivity of
every Stage A conclusion to KV size for the cost of one extra arm, and that
sensitivity is the single most useful output of the whole exercise.

### Selection

- **Both families are in the matrix.** Gemma is the capability reference and the
  KV-expensive extreme; Qwen is the KV-cheap extreme.
- **Laptop arms lead with Qwen.** If a 1.9 GB checkpoint does not fit or does
  not restore in time on the laptop, that is itself a result — record it and do
  not treat the arm as failed.
- **The AI machine runs the asym-vs-Q8_0 comparison** you asked for, on both
  families.

### What asym-vs-Q8_0 actually isolates — and what it does not

Weight quantisation changes **weights only**. KV cache size is a function of
architecture and `type_k`/`type_v`, not of weight quant. So across an
asym → Q8_0 pair:

- **Blob size, save time and restore time should be identical.** If they are
  not, an assumption is wrong and that is worth finding.
- **Prefill ms/token should change**, because weight bandwidth dominates
  prefill.

That makes it a clean controlled experiment: it moves the prefill term of the
break-even inequality while holding the restore term fixed. Both are needed and
neither can be inferred from the other.

### Measurement matrix

| Arm | Model | Quants |
|---|---|---|
| AI machine, GPU | gemma-4-26B-A4B | asym v15, Q8_0 |
| AI machine, GPU | qwen3.6-35B-A3B | asym, Q8_0 |
| AI machine, CPU | both | asym only |
| Laptop, GPU | qwen3.6-35B-A3B | asym |
| Laptop, GPU | gemma-4-26B-A4B | asym (expected to strain; record the outcome) |
| Laptop, CPU | qwen3.6-35B-A3B | asym |

**Not DeepSeek-V4-Flash on any arm** — its GPU output is broken by the vendored
llama.cpp's age.

**Prerequisite:** confirm the Q8_0 GGUFs on the AI machine are the same A4B MoE
checkpoints as the asym builds. Only `gemma-4-26B-A4B-it-UD-IQ2_XXS.gguf`
carries the `A4B` tag; the Q8_0 and q4_0 files are named `gemma-4-26B-it-*`. If
they are different base checkpoints the comparison is void.

---

## The six proposals — verdicts

| # | Proposal | Verdict |
|---|---|---|
| 1 | Single-active-slot KV swapper | **Already the architecture.** One context per role, every batch on sequence `0`. Collapses into the disk tier itself. |
| 2a | Radix index over `prefix_tokens` | **Defer.** Linear scan costs ~1 ms against a miss costing minutes. |
| 2b | Fork sequence 0 with `kv_cache_seq_cp` | **Real, and gated.** Bindings expose it; Fono uses none of it. Blocked on a recurrent-rewind correctness fix. |
| 3 | `madvise` on MoE experts | **Not exposed, unproven.** Falsify cheaply before any code. |
| 4 | Pin non-expert layers | **Same as 3.** `use_mlock` is all-or-nothing and off on purpose. |
| 5 | `flash_attn = true` | **Already on, and now guarded.** Default is `AUTO`, and a quantized V cache *requires* it — context creation returns `nullptr` otherwise. Fono runs `q8_0` V and loads, therefore FA is enabled; Task 1 asserts the pairing at creation. |
| 6 | Rolling summary eviction | **Out of scope for goal 2**, and the summary half is hostile to caching. |

Two points worth repeating because they change the design:

- **A rolling summary rewrites the front of the prompt**, invalidating every
  cached prefix at once — the exact divergence failure previously identified as
  the dominant cause of misses. A front-of-prompt summary and a prefix cache are
  in direct opposition.
- **Fono does not own the coding agent's context.** The client sends the whole
  conversation over HTTP. Pruning it without telling the client is lying about
  what was seen. Cap `n_ctx`; let clients manage their own window.

---

## The one thing that decides everything

The economics were never in doubt — break-even disk bandwidth is single-digit
MB/s against prefill costing minutes.

**The open question is whether a real coding agent's prompt prefix is stable
turn to turn.** If the client injects a file tree, timestamp or shuffled tool
list at the front of every request, the deepest match is near zero and the tier
stores checkpoints nothing ever reads. Measurable in an afternoon, and it gates
the whole build.

---

## Implementation Plan

### Stage A — Instrument and measure (no feature code)

- [x] Task 1. Assert flash attention is enabled at context creation and record
      the effective policy in the doctor line. Closes proposal 5 with a guard,
      and makes a future upstream default flip loud instead of silent.
      **Done.** `flash_attention_policy` in `crates/fono-core/src/llama_backend.rs`
      refuses a quantized value cache paired with the policy explicitly off, and
      names the policy otherwise; every assistant context creation calls it and
      records the answer on `llm.context_created`. `fono doctor` prints it under
      Compute backends. `auto` is reported rather than resolved — llama.cpp
      decides at creation and does not publish the answer, so a successful load
      with a q8_0 V cache is itself the evidence it resolved to on.
- [x] Task 2. Extend the per-lookup cache trace into a **miss taxonomy**: for
      each lookup record the deepest matched token position and why it was not
      deeper — capacity eviction, prefix divergence (with divergence index and
      the prompt segment it lands in), or runtime-key change. Count **decoded
      prefix tokens**, never hit counts.
      **Done.** `PromptStateCache::explain_longest_prefix` returns the match plus
      one of four causes — `deepest`, `eviction`, `divergence`, `runtime_key_change`
      — and the assistant emits it as `llm.prompt_cache_lookup` on every lookup,
      hit or miss, counted in decoded prefix tokens.

      Telling eviction from divergence needs memory a live cache does not have:
      both look like a shallow match. So eviction now leaves a **tombstone** —
      the dropped entry's token vector and blob size, no blob — and a later
      lookup re-runs prefix matching against those. A tombstone that reaches
      past the live match names the tokens and bytes a disk tier would have
      recovered, which is precisely the Stage C case. The ring is capped at
      256k tokens (1 MiB) so the diagnostic cannot become a second cache.
- [x] Task 3. Add a `fono-bench` mode driving the HTTP surface as a coding
      client does — a growing conversation carrying tool results and file
      contents, to 16k tokens.
      **Done**, as `fono-bench coding-client-cache`: it POSTs a whole
      conversation to Fono's OpenAI-compatible endpoint each turn, appending a
      real file's contents as the turn's tool result, until it reaches
      `--target-tokens`. Client-side it records wall time and time to first
      token; what each turn cost the cache is Task 2's daemon-side trace.

      One caveat to read the results with: this conversation is **append-only by
      construction**, which is the friendliest possible shape for a prefix
      cache. It measures the ceiling, not a real client. If checkpoints are not
      reused here they will not be reused anywhere, but reuse here does not by
      itself settle Task 10.
- [ ] Task 4. Confirm the AI machine's Q8_0 GGUFs are the same base checkpoints
      as the asym builds. If not, either obtain matching ones or drop the
      asym-vs-Q8_0 arm and say so.
- [x] Task 5. Run the measurement matrix above. Per cell:
      - prefill ms/token at 1k, 4k, 8k, 16k
      - KV bytes/token, measured, cross-checked against the estimator in
        `crates/fono-core/src/gpu_offload.rs`
      - 16k blob size
      - `state_get` (save) and `set_state_data` (restore) wall time at that size
      Un-niced — these are timed measurements.
      **Done.** 16/16 cells on the AI machine, 14/16 on the laptop (its two
      gemma-on-device cells at 8k and 16k abort on device memory). Estimator
      cross-check agrees to 0.02% on gemma; it under-predicts qwen's fixed cost
      by 66 MB. Results and per-cell logs under `/mnt/150g/stage-a/results/`
      and `/root/test/stage-a/results/`.
- [ ] Task 6. Within each asym/Q8_0 pair, assert blob size and restore time are
      unchanged and prefill is not. A violation invalidates an assumption and is
      the more interesting outcome.
- [x] Task 7. Measure restore **with and without GPU offload** at 16k.
      **Done, and the premise is retracted.** The earlier 130 → 269 ms
      "offload doubles restore" does not reproduce: device restore is *faster*
      than host restore at 16k for both models on both machines. Nothing
      supports sizing the tier around an offload penalty.
- [x] Task 8. Measure the storage path: sequential read and write throughput on
      the resolved cache directory per machine, and whether that directory is
      **memory-backed**. `/root` is tmpfs on at least one target, where a naive
      tier consumes RAM while believing it used the SSD.
      **Done, both halves.** Re-measured on idle machines with `O_DIRECT` after
      dropping caches, 4 GiB working set:

      | | link | write | read QD1 1M | read QD1 16M | read QD4 1M |
      |---|---|---|---|---|---|
      | laptop `/mnt/150g` | PCIe 5.0 x4 | 7.0 GB/s | 8.4 GB/s | **13.4 GB/s** | 13.5 GB/s |
      | AI `/root` | PCIe 4.0 x4 | 5.4 GB/s | 4.7 GB/s | **7.0 GB/s** | 7.05 GB/s |

      Both drives sit at their link ceiling (≈85 % of 15.75 GB/s and ≈89 % of
      7.88 GB/s), so the storage is not the limit and there is no headroom left
      to find. Block size matters more than queue depth: 16 MiB reads are 1.6×
      (laptop) and 1.5× (AI) faster than 1 MiB at the same QD1, while going from
      QD1 16M to QD4 buys nothing. **The tier should read a checkpoint in large
      sequential chunks, not many small ones**; a threaded reader is
      unnecessary. The earlier 4.1 / 2.4 GB/s figures are **retracted**: they
      were taken while a 12 GB model upload and a benchmark cell were competing
      for the same devices. Against a worst-cell break-even of 104 MB/s the
      margin is 129× (laptop) and 67× (AI), not 23–40×. Neither resolved
      cache directory is tmpfs (`/mnt/150g` ext4, AI `/root` ext4), so no target
      trips the refusal today. The refusal is still required: it guards a
      configuration a user can create, not one we happened to measure.
- [ ] Task 9. Falsification test for proposals 3 and 4: with a model that does
      **not** fit RAM, record major fault counts and tok/s during generation
      against the same model resident. If paging is not a material share of the
      token budget, both proposals die for free.
      **Half-answered, and not the interesting half.** Major faults were **0 in
      all 30 completed cells**, but every cell used a model that fits RAM on that
      machine, which is the case where both proposals are trivially inert. The
      test still needs a model larger than RAM to say anything. What this does
      establish: neither proposal can help the configurations Stage A covered,
      so any benefit is confined to the over-committed case.
- [x] Task 10. **Gate — resolved yes on 2026-08-16. Build Stage C.**
      Six conversations over the OpenAI-compatible endpoint, at a budget that
      holds three checkpoints. Every revisited conversation whose checkpoint had
      been dropped reported `eviction`: **32,435 prefix tokens re-decoded across
      three turns, against 5 for every other cause combined.** The one
      conversation whose checkpoint survived answered in 4.3 s; the three that
      had lost theirs took 76, 80 and 114 s for the same shape of question.
      A daemon restart then cost a further 4,673 tokens that the taxonomy cannot
      attribute at all, because an empty cache leaves no tombstone — the case
      for disk that no in-memory measurement can make.
      The budget bound on **bytes**, not on `max_entries`, so the hard-coded 10
      is not a cheaper alternative here.
      Full write-up: `plans/2026-08-16-task-10-gate-resolved.md`.
- [x] Task 10a. **Precondition to the gate.** Size the in-memory cache budget
      against available RAM instead of the hard-coded 256 MiB both backends
      used. `PromptStateCache::sized_for_host` takes a quarter of free RAM
      after the desktop's 4 GiB reserve, floored at the old 256 MiB and capped
      at 8 GiB (`crates/fono-core/src/prompt_cache.rs:604-643`); both embedded
      backends now build with it. Free RAM is the ceiling — it says what the
      cache *may* spend, never what it *should*.
- [x] Task 10b. **The budget is counted in checkpoints, not gigabytes.** A
      checkpoint is a copy of the KV cache llama.cpp already allocated, so its
      size is `kv_bytes(n_ctx, type_k, type_v)` — the quantity the offload
      decision already computes, validated to 0.02 % on gemma in the sweep.
      Below one checkpoint the cache provably retains nothing; two or three is
      what makes a longest-prefix match against a predecessor possible. The
      budget is now `3 × kv_bytes`, clamped by Task 10a's RAM ceiling, applied
      once the model is loaded and its context known. If one checkpoint does
      not fit under the ceiling the cache is disabled and says so, rather than
      thrashing. **Measured live** (laptop, 30 GiB RAM / 21 GiB available,
      gemma-4-e2b q4_0 at ctx=20480): checkpoint 446 MB, budget 1338 MB — the
      3× term binds, the RAM ceiling (≈4.3 GB) does not.
      **`max_entries` is still a hard-coded 10 and is deliberately unsized:**
      no measurement says what it should be, and a second guess beside the one
      just removed would be worse than leaving the trap documented. See the
      warning under "How to run the gate".

### Stage B — Only if divergence dominates

- [ ] Task 11. Order the prompt stable-material-first, targeting whichever
      segment Task 2 attributes the divergence to. Free, and converts a total
      invalidation into a partial one.
- [ ] Task 12. Give the HTTP surface a warm path. It has the most conversations
      and the least cache help; pinning currently requires empty history, so a
      client on turn 5 never pins.
- [ ] Task 13. Re-run the Task 10 gate. A disk tier is worth building only
      against a prefix that survives.

### Stage C — The disk tier, sized by Stage A

- [x] Task 14. ADR before code, matching the posture of ADR 0040: 0600
      permissions, finite retention, an opt-out that creates **no file at all**,
      and an explicit delete control. A KV blob is a materialisation of the
      transcript. — `docs/decisions/0042-prompt-checkpoints-on-disk.md`. Files
      are 0600, the directory 0700, `prompt_cache_gb = 0` creates nothing.
- [x] Task 15. Refuse to enable the tier when the cache directory is
      memory-backed, and report the refusal in `fono doctor`. — the refusal
      fired for real during testing, on a tmpfs `/tmp`.
- [x] Task 16. Key persistent entries on an explicit `STATE_FORMAT_VERSION`
      composed with the binding version, and on the **GGUF content hash** rather
      than model mtime. Decouple from `CARGO_PKG_VERSION` — as written today
      every point release would discard the whole cache, defeating goal 1. —
      one `runtime_identity()` now feeds both the in-memory key and the disk
      tier; it carries no `CARGO_PKG_VERSION`.
- [x] Task 17. On-disk format: header with magic, format version, runtime key,
      token count, payload length, payload checksum; then payload; then the
      `prefix_tokens` vector. The token vector is **mandatory** — an entry that
      loses it drops out of longest-prefix matching entirely.
- [x] Task 18. One content-addressed file per entry, published `.part` → verify
      → rename, mode 0600.
- [x] Task 19. Write-back on eviction, never write-through on insert, skipping
      the write when the content-addressed file already exists. At 16k, write
      amplification is the dominant cost of the feature — and 11× worse on Gemma
      than on Qwen, which is why Task 5 measures both. Also written on clean
      shutdown, which is the trigger the gate's restart finding argued for.
- [ ] Task 20. All disk I/O off the model mutex: reads resolved before the lock
      is taken, writes on a niced background thread. A multi-gigabyte write under
      the model lock stalls every other conversation.
      **Deferred deliberately.** A save costs about what a restore costs —
      ~200 ms on qwen, ~1 s on gemma — against turns of 4 to 114 seconds, so
      0.2–5 %. Writes are synchronous until a measurement says otherwise.
- [x] Task 21. Quarantine on failure — any header, checksum or `set_state_data`
      rejection deletes the file and records a miss; never retry a bad blob.
      Wire the degenerate-output guard to delete the backing entry, or a poisoned
      checkpoint survives restarts.
- [x] Task 22. Retention: LRU against a byte cap, plus deletion of entries whose
      runtime key is no longer current, plus a hygiene sweep for anything
      untouched beyond a fixed age. Swept at startup alongside the existing
      history purge, not on a timer.
- [x] Task 23. One config key, absent by default, `0` meaning disabled. A test
      fails the build if a second knob appears. — `prompt_cache_gb`.
- [x] Task 24. Confirm no net-new dependency, and **add no compression crate** —
      KV data compresses poorly and binary size is the standing constraint.

### Stage D — Sequence forking, gated on a correctness fix

- [ ] Task 25. Settle the recurrent-state rewind first. The binding documents
      `PARTIAL_ONLY` as the **only** correct rewind for Gated Delta Net models —
      which `gemma-4` is — while Fono's post-generation truncation uses
      `clear_kv_cache_seq`. Establish whether that corrupts partial state, and
      if so replace it with `state_seq_get`/`state_seq_set` under
      `PARTIAL_ONLY`. This is a correctness defect in its own right; persisting
      on top of a broken rewind turns transient drift into stored drift.
- [ ] Task 26. Measure `kv_cache_seq_cp` fork cost against a full-state restore
      at 16k, and how many 16k heads fit inside one `n_ctx` per arm. On Gemma
      that is a far harsher constraint than on Qwen, so measure both.
- [ ] Task 27. Adopt sequence forking only if Task 26 shows both a materially
      cheaper fork and room for more than one head.

### Stage E — Deferred, explicitly

- [ ] Task 28. Radix index over `prefix_tokens`, only once an entry-count
      measurement shows the linear scan is visible against the miss cost.
- [ ] Task 29. Per-tensor `madvise` / partial `mlock`, only if Task 9 shows
      paging is a material share of the token budget. Treat as an upstream
      contribution, not a local patch.

---

## Stage A execution — how to resume

The runner is `stage-a-run.sh`, unattended and idempotent: one JSON per
(arm × model × prefix size), and a step whose JSON exists is skipped. After a
crash, re-launch the same script and it continues. Never niced — these are
timings.

| Machine | Base dir | Launch |
|---|---|---|
| laptop | `/mnt/150g/stage-a` | `./launch.sh laptop` |
| AI machine | `/root/test/stage-a` | `/root/test/stage-a-launch.sh ai` |

Progress is `logs/runner.log`; completion is `results/runner.done`. Both
machines run at once — they share nothing but the LAN.

**The matrix is complete: 16/16 cells on the AI machine, 14/16 on the laptop**
(its two gemma-on-device cells at 8k and 16k abort on device memory). Nothing in
Stage A's measurement matrix is left to run. Two operational notes for whoever
picks this up:

- The AI machine's launcher refuses to start a second runner over the same
  directory, so the auto-relaunch that fires when a model finishes transferring
  cannot race a runner already working.
- `rsync` without `-L` silently skips a symlink, which is how the gemma push
  appeared to succeed while transferring nothing. The model paths under
  `/mnt/150g/fono-cache/models/` are symlinks.

Reach the AI machine with `ssh -n -o BatchMode=yes` under a hard `timeout`. A
plain `ssh` blocks indefinitely when that box is loaded, and without `-n` it
holds the channel open and the wrapper's timeout never fires.

Sizes go 1k → 4k → 8k → 16k and the small model before the large one, so an
interrupted run still leaves a usable curve rather than one arm at full size.

Models are the two asymmetric quants, capability-matched on the ten-task graded
suite (gemma 1.0 at every tier, qwen 0.9–1.0 — one task, inside this suite's
resolution). The AI machine also gets the `Q8_0` variants, which raise prefill
without changing KV geometry, so they bound the economics from above.

## How to run the gate (Task 10)

Everything is built; this is an operator step, not a code step.

1. In `config.toml`, set `[server.llm] enabled = true`, `auth = false`, and
   raise `[assistant] context` to something a coding conversation reaches.
2. Start the daemon with the taxonomy visible:
   `FONO_LOG=info,fono_assistant::llama_local=debug fono`.
3. Point a coding agent at `http://127.0.0.1:11434/v1/chat/completions` and
   work normally — several conversations, and at least one daemon restart.
4. Read the result. Either the web settings cache panel (Health → Prompt
   cache), which now totals re-read tokens by cause in plain words and says
   what one conversation costs against the budget, or
   `grep 'prompt cache lookup' <log>` for the per-lookup detail. `fono doctor`
   names the eviction total on its own line when there is one.

**Sum the decoded prefix tokens per cause; do not count lines.** A single
`eviction` on a 16k prompt outweighs a hundred hits on short ones, which is
why the taxonomy reports tokens.

**One trap when reading it.** The cache bounds entries *and* bytes, and Task
10a only sized the bytes. The entry cap is still a hard-coded 10
(`crates/fono-core/src/prompt_cache.rs:604-643`), so a session juggling more
than a handful of conversations can report `eviction` because of that constant
rather than because of memory — the same mistake the 256 MiB budget invited. If
eviction dominates, check the entry count before concluding anything about
disk: raising a constant is cheaper than building a storage tier.

## Stage A findings so far

Measured, 1k prefix, both laptop arms. Five corrections to assumptions this plan
was written on:

**Read this before any number below.** The `vk` arm means *the binary links
Vulkan*, not *the model ran on the device*. The verdict has to be read out of
each run's log, and it is not the same across models:

| arm | model | layers offloaded | what it actually is |
|---|---|---|---|
| `vk` | qwen3.6-35B | **41/41** | a genuine device run |
| `vk` | gemma-4-26B | **0/31** | Vulkan *compute*, weights and KV in host RAM |
| `cpu` | either | n/a, no Vulkan linked | host only |

The sizing policy refused gemma on this iGPU — 8.9 GB of weights plus 2.34 GB of
KV at `n_ctx` 20480 plus the working allowance exceeds the budget once the 4 GiB
desktop reserve is taken. So there is **no device-resident gemma arm on the
laptop**, and any gemma "GPU vs CPU" comparison here is Vulkan-compute vs
host-compute. Labelling that as offload was a mistake and the figures below are
corrected for it.

| arm | model | blob B ÷ token at 1k | prefill ms/token | restore ms | outputs match |
|---|---|---|---|---|---|
| vk (0/31) | gemma-4-26B asym | 119,705 | 13.77 | 1039 | no — see below |
| cpu | gemma-4-26B asym | 119,705 | 42.06 | 1139 | no — see below |
| vk (41/41) | qwen3.6-35B asym | 78,872 | 13.30 | 170 | yes |
| cpu | qwen3.6-35B asym | 78,872 | 30.91 | 218 | yes |

1. **The estimator is right.** `gpu_offload.rs` predicts 119,680 B/token for
   gemma; measurement says 119,705, an error of 0.02 %. The sizing that offload
   already depends on is confirmed against a second, independent method.
2. **The 11.7× blob-size spread was right in kind and wrong in size, and the 1k
   snapshot above understates it.** Regressing blob size on token count across
   the sweep separates the two terms:

   | model | marginal | fixed | projected 16k blob |
   |---|---|---|---|
   | gemma-4-26B | 119,704 B/token | **0** | **~1.96 GB** |
   | qwen3.6-35B | 10,900 B/token | **65.9 MB** | **~245 MB** |

   gemma is purely linear — every layer keeps per-token attention state. qwen's
   hybrid layers charge a large fixed recurrent state and almost nothing per
   token, which is why it looks 1.5× at 1k and **8×** at 16k. Both terms matter
   and neither model shows both, so keeping both arms is what makes the design
   general rather than fitted.
3. **Offload does not clearly change restore either way.** It was believed to
   double it. Only qwen has a device-resident arm, and it goes both ways there:
   170 ms on the device against 218 on the host at 1k, then 269 against 219 at
   4k. A wash within ±25 %, not a doubling. The standing 2× figure does not
   survive, and no gemma evidence exists on this machine.
4. **Vulkan cuts prefill 2.2–3.1× on gemma with zero layers offloaded.** This is
   the most useful thing the laptop produced and it was nearly written up as
   offload. With `0/31` layers on the device, llama.cpp still schedules the graph
   across it — `graph splits = 695`, `Vulkan0 compute buffer = 1282 MiB` — so
   compute runs on the GPU while weights and KV stay in host RAM. 42.06 → 13.77
   ms/token at 1k, 166.0 s → 75.6 s at 4k.

   This works because the iGPU shares system memory, so there is no bus crossing
   to pay per token. It is the opposite of the earlier finding that *partial layer*
   offload measured slower than none, and not in conflict with it: moving some
   layers costs a crossing per token, moving none and computing on the device
   costs nothing extra. **This must not be generalised to a discrete card**, where
   the same configuration would stream every weight across PCIe.

   It also means a machine too small to hold a model still gains from the
   accelerator, which the current all-or-nothing policy does not exploit — the
   policy returns 0 layers and that is correct, but linking Vulkan is worth it
   anyway. Untested on the AI machine; that arm decides whether it generalises.
5. **Prefill scaling runs in opposite directions.** gemma costs *more* per token
   as the prefix grows (13.77 → 21.43 ms/token, 1k → 4k, Vulkan arm) while qwen
   costs *less* (13.30 → 8.07). So the tier's value grows with context on gemma
   and shrinks on qwen, and a break-even quoted at one size does not transfer.

Two results that stand on their own:

- **Zero major faults in every run.** This is the free falsification test for
  proposals 3 and 4: with no paging there is no read-ahead problem for `madvise`
  or `mlock` to solve. Both models fit RAM on this machine, so the test must be
  repeated on the AI machine's `Q8_0` arm before the proposals are declared dead.
- **gemma's output is degenerate on this prompt, cached or not.** The
  `outputs_match: false` column above is **not** evidence of a state-restore
  defect, and reading it that way was a mistake. Both replies are loops —
  uncached: `comparison-based? comparison-based? comparison-based?`; cached:
  the same collapse down a different branch. Two degenerate samples diverge
  because degenerate output is unstable, so the column carries no information
  for this model. What *does* carry information is the round trip, and it is
  clean: `state_roundtrip_diff_bytes = 0` on every gemma run. qwen answers the
  same prompt coherently on both arms and matches exactly.

  So the recurrent-rewind risk is still **untested**, not observed. Testing it
  needs a prompt gemma answers coherently; that is a harness fix, and until it
  lands no gemma correctness claim can be made in either direction.

  At 4k the gemma GPU arm **does** match, which is the confirmation this reading
  needed: the mismatch is a property of the 1k prompt, not of the state.

### 4k, and the economics

| arm | model | prefill | restore | ratio |
|---|---|---|---|---|
| vk (0/31) | gemma-4-26B | 75.6 s | 0.831 s | **91×** |
| cpu | gemma-4-26B | 166.0 s | 1.087 s | **153×** |
| vk (41/41) | qwen3.6-35B | 25.5 s | 0.269 s | **95×** |
| cpu | qwen3.6-35B | 101.4 s | 0.219 s | **463×** |

**The measured restore does not include a disk read.** The harness restores from
an in-memory `Vec<u8>` via `set_state_data`, so these figures are the
memory→context half only. A disk tier pays that *plus* the file read, and the
two compose: restore throughput lands at 373–508 MB/s, i.e. `state_set` is
already memory-bandwidth-bound, so the disk read is additive, not hidden behind
it. Worst measured case is gemma at 4k — a 422 MB blob, so roughly +0.8 s on a
500 MB/s device, against 75.6 s of prefill avoided. The margin absorbs it
whole, and would absorb a medium ten times slower.

Every quoted prefill number is one cold prefill of the *whole* prefix. That is
the right comparison for goal 1 (restart) and for a cold client turn, and it is
the wrong one for a warm turn already holding the prefix in RAM — the tier does
not compete with the RAM tier, it backs it.

**Caveat on the laptop timings:** the 11.7 GB upload to the AI machine ran
concurrently with these runs, at ~4 MB/s. Major faults stayed at 0 throughout,
which is the direct evidence that it did not evict model pages, but the CPU-arm
numbers should be treated as an upper bound on cost until a spot re-run confirms
them on an idle machine.

Economics at 1k are already lopsided: on the Vulkan arm restore beats re-prefill
by 14.5× on gemma and 76× on qwen. The read side was never the question; the
prefix-stability gate still is.

### One crash, and what it forecloses

`gemma-4-26B` at 8k on the Vulkan arm **aborts** — SIGABRT, `fatal runtime error:
Rust cannot catch foreign exceptions`, 192 s in, after the scheduler reserve
logged cleanly and with no llama.cpp error line before it. The same model at 1k
and 4k is fine, and qwen reaches 8k on both arms.

Two things follow, one of them a defect in shipped code:

- **A foreign exception in the backend takes the whole process down.** There is
  no `catch_unwind`-equivalent for a C++ throw crossing back into Rust, so the
  daemon dies rather than falling back. For a bench harness that is a lost cell;
  for the daemon it is a crash on a large prompt. Whatever the root cause, the
  fallback path needs to exist.
- **It bounds the 16k claim on the Vulkan arm only.** The CPU binary completed
  gemma at 16k on the same machine (15,319 tokens, 1.83 GB blob, restore 933 ms),
  so the headline blob size is measured, not extrapolated. What is unmeasured is
  16k gemma *with a device*, on this hardware, in any configuration.

**Attributed, after one wrong attribution was retracted.** The exception is
`vk::ErrorDeviceLost` — the GPU is lost mid-submit, not an allocation refusal.
The abort is deterministic and monotone in prefix length: gemma passes 1k and
4k, fails 8k and 16k. The CPU binary completes both on the same model (8k in
854 s; 16k in 1036.2 s, 15,319 tokens, restore 933 ms), so it is specific to the
Vulkan build.

Eight cells, varying batch size and layer residency independently:

| batch | layers | splits | 8k | 16k |
|---|---|---|---|---|
| 20480 | 0/31 | 3 | abort | abort |
| 20480 | 31/31 | 2 | — | abort |
| 2048 | 31/31 | 2 | **ok** | abort |

**`op_offload` is not the cause, and the earlier claim that it was is
withdrawn.** With all 31 layers resident and only 2 graph splits — nothing left
for `op_offload` to ship — 16k still aborts. So no `op_offload` setter is needed
for this, and the second upstream contribution the plan proposed is off the
table. What the sys binding exposes and the safe API does not is unchanged and
still true; it is simply not the fix here.

What the matrix does establish:

- **A smaller batch buys headroom, not immunity.** `--batch-size 2048` rescues
  8k at full offload and does nothing for 16k.
- **The trigger is submit size, not resident weights.** Both the 0/31 and 31/31
  arms die at 16k.
- **Device loss, so a fallback cannot be a `catch`.** Once the device is lost
  every subsequent submit on that context fails. Recovery means a fresh context,
  host-side, from the same prompt.

So a user with an `accel-*-vulkan` build on this integrated GPU crashes on any
gemma-class prompt past roughly 8k, whether or not the model fits the device.
That bounds the 16k headline on this hardware to the CPU arm, and it is a
release-blocking defect for the Vulkan builds, independent of the disk tier.

Unresolved: whether this is a Mesa/driver hang or a llama.cpp Vulkan-backend
defect. Reproducing under upstream `llama-cli` at the same batch and prefix is
the next step, and the standing rule from the DeepSeek episode applies — reach
for `llama-cli` before blaming a backend.

### Full sweep — the economics are settled

Both machines, every completed cell. `ctx = 20480` throughout, one iteration per
cell, prefixes cut from Fono's own source tree.

Hosts. Laptop: 8 cores, 32 GB, cache dir reported as ext2/ext3 by `stat`, 4.1 GB/s write and 10.9 GB/s
read, not memory-backed. AI machine: 32 cores, 32 GB visible (96 GiB carved out
to the device in firmware), cache dir reported as ext2/ext3, 2.4 GB/s write and 3.6 GB/s read,
not memory-backed. Neither cache directory is tmpfs, so the memory-backed
refusal the plan asks for is not exercised by these runs.

| Host | Arm | Model | Prefill marginal | Blob at 14k | Restore |
|---|---|---|---|---|---|
| laptop | CPU | qwen3.6-35B | 41.6 ms/tok | 218 MB | 77–219 ms |
| laptop | Vulkan | qwen3.6-35B | 9.04 ms/tok | 218 MB | 100–269 ms |
| laptop | CPU | gemma-4-26B | 51.5 ms/tok | 1.83 GB measured at 15,319 tok | 0.93–1.21 s |
| laptop | Vulkan | gemma-4-26B | 24.9 ms/tok | — (aborts at 8k) | 0.83–1.04 s |
| AI | CPU | qwen3.6-35B | 11.9 ms/tok | 218 MB | 64–77 ms |
| AI | Vulkan, 41/41 layers | qwen3.6-35B | **1.26 ms/tok** | 218 MB | 50–60 ms |

Three findings, then the number that closes the economics question.

**The shipped KV estimator is accurate.** `crates/fono-core/src/gpu_offload.rs`
predicts 119,680 B/token for gemma; measured marginal is **119,704** — 0.02%
out. For qwen it predicts ~10,240 against a measured 10,900, 6% out and on the
conservative side. That estimator is what the offload sizing decision rests on,
so this is a validation of shipped code, obtained free.

**Restore does not scale with conversation length.** This is the load-bearing
correction. Restore is flat per model: 50–77 ms for qwen across a 3× range of
blob sizes, 0.83–1.21 s for gemma. The apparent MB/s therefore *rises* with
blob size, which is an artefact of dividing a growing numerator by a constant —
not a bandwidth measurement. Restore cost tracks the **allocated** cache at
`ctx = 20480`, not the tokens in use. A tier's restore cost is bounded by the
context setting and does not grow as a conversation does.

**The two models bracket the design, but not the way the plan assumed.** Gemma
is pure attention: 119,704 B/token marginal, **zero** fixed. Qwen is hybrid:
10,900 B/token marginal on top of a **65.9 MB** floor that exists at one token,
because the recurrent layers carry constant state. So per-token cost is 11×
apart while at 1k the blobs are within 1.7× — the ratio is a function of length,
and quoting a single "11.7× apart" figure was wrong.

**Break-even disk bandwidth, per arm.** Blob bytes divided by the prefill
seconds a restore avoids:

| Arm | Break-even | Margin vs measured medium |
|---|---|---|
| AI, Vulkan, qwen (fastest prefill) | 12.6 MB/s | **285×** |
| AI, CPU, qwen | 1.4 MB/s | 2,570× |
| laptop, Vulkan, qwen | 1.7 MB/s | 6,400× |
| laptop, CPU, gemma at 8k | 2.4 MB/s | 4,500× |

The worst case is the arm where prefill is *cheapest*, and it still wins by two
and a half orders of magnitude. **The economics question is closed.** No further
measurement of the read side is warranted. What remains is entirely the
prefix-stability gate: whether a real coding client's prompt prefix is stable
enough turn to turn for a stored checkpoint ever to be matched.

### Two findings outside the cache question

Both bear on code already shipped, and neither was being looked for.

**A Vulkan device accelerates prefill without holding the model.** Gemma on the
laptop offloads **0 of 31 layers** and keeps its whole KV cache on the host — the
sizing policy correctly refuses it — yet prefill is 3× faster than the CPU arm
(24.9 against 51.5 ms/token). The mechanism is llama.cpp's `op_offload`
(`include/llama.h:391`, on by default): a 1,282 MiB Vulkan compute buffer is
allocated and large prompt matmuls are sent to the device per operation, with
weights staying in host memory. This matters for the offload work just
committed, which frames the fallback as "everything stays on the processor,
which is slower". That is true of decode and false of prefill. The all-or-nothing
verdict is unaffected — this is not partial offload, no layer is resident — but
the cost of refusing is lower than assumed, which strengthens the policy rather
than weakening it.

**Restoring a saved cache onto an AMD device changes the reply; onto an Intel
device it does not.** Every AI-machine Vulkan cell diverges from its uncached
baseline; every CPU cell on both hosts matches; every laptop Vulkan qwen cell
matches. Same binary, same model, same context, same prompt. Divergence is
reproducible — two uncached runs are byte-identical to each other, two cached
runs likewise, and the split occurs at the same offset every time — so it is not
run-to-run nondeterminism. The serialised bytes round-trip with
`state_roundtrip_diff_bytes = 0`, so nothing is lost in save or load.

The discriminator is the device: Intel Lunar Lake iGPU with 41/41 layers
resident reproduces exactly, AMD RADV GFX1151 with 41/41 layers resident does
not. The likely cause is that restoring a cache does not reproduce the exact
numerics of computing it in place on that driver, in a way Intel's does not
expose.

This is a property of the **in-memory** cache Fono ships today, not of the
proposed disk tier — the tier would store the same bytes and hand back the same
buffer. It does mean the tier cannot be validated by output equality on that
host, and it deserves attribution independently of this work.

Gemma's mismatches are a separate and duller thing, already retracted once
above: it degenerates into repetition on this prompt with **no** cache involved,
so cached-versus-uncached comparison on gemma is uninformative either way.

### The shipped sizing estimator is accurate to 0.2 %

The sweep gives the estimator in `crates/fono-core/src/gpu_offload.rs` its first
independent check, against blob sizes it never sees. Predicted per-token cost
comes from reading the GGUF key-value table; measured cost is the slope of
`state_bytes` across the four prefix lengths.

| Model | Predicted | Measured slope | Error |
|---|---|---|---|
| gemma-4-26B-A4B asym | 119,680 B/token | 119,704 B/token | **+0.02 %** |
| qwen3.6-35B-A3B asym | 10,880 B/token | 10,900 B/token | **+0.18 %** |

Both arms of each model agree to the byte, which is expected — KV geometry is
fixed by architecture and untouched by where the layers run. The residual ~20
B/token is state-blob framing, not a modelling error.

This matters beyond the cache: the offload decision already shipped rests on
this estimate, and until now nothing had checked it against a real allocation.

**One gap it does expose.** The estimator models per-token cost only, and
qwen carries a **65.9 MB fixed** component it does not predict — at 969 tokens
that is 86 % of a 76.5 MB blob. Gemma's fixed component is **0.0 MB**. The
models differ in exactly the way that explains it: gemma is 30 blocks, all
attention; qwen is 40 blocks, mostly recurrent, and a Gated Delta Net state is
per-layer and independent of context length. So the fixed term is the recurrent
state, and the estimator omits it by treating recurrent blocks as costing
nothing per token — correct per token, wrong in total.

Against a multi-gigabyte offload budget 66 MB is noise, so this is not a defect
in the shipped decision. It is a real gap for the tier, where it is the
difference between a 10 MB and a 76 MB smallest-possible checkpoint.

### The complete grid, and the break-even the tier must clear

The AI machine finished **16 of 16 cells with no failures**, including gemma on
the device at 8k and 16k — 31/31 layers offloaded, KV resident on the device
(2125.00 + 212.50 MiB). The laptop finished 14 of 16; its two gemma-on-device
cells at 8k and 16k abort with `rc=134`.

That pairing settles the abort: the **same binary and the same model succeed on
the larger device**, so it is a device-memory ceiling on the laptop's iGPU, not
a code defect. It still needs a graceful refusal rather than an abort, but it is
a sizing gap, not corruption.

Prefill and restore at 16k, both machines, per arm:

| Machine / arm | Model | Prefill | Restore | Blob |
|---|---|---|---|---|
| AI, device | gemma-4-26B | 17.9 s | 328 ms | 1833.7 MB |
| AI, host | gemma-4-26B | 194.0 s | 487 ms | 1833.7 MB |
| AI, device | qwen3.6-35B | 17.3 s | 60 ms | 218.2 MB |
| AI, host | qwen3.6-35B | 161.4 s | 77 ms | 218.2 MB |
| laptop, host | qwen3.6-35B | 699.7 s | 323 ms | 218.2 MB |
| laptop, device | qwen3.6-35B | 130.5 s | 171 ms | 218.2 MB |

**Break-even disk bandwidth** — the read rate at which restoring stops being
cheaper than re-prefilling, `blob / (prefill − restore)`:

| Cell | Break-even |
|---|---|
| AI device, gemma 16k | **104 MB/s** ← binding constraint |
| AI device, qwen 16k | 12.7 MB/s |
| AI host, gemma 16k | 9.5 MB/s |
| laptop host, gemma 8k | 2.4 MB/s |

The worst case across the whole grid is **104 MB/s**, against measured cache-dir
throughput of 4.1 GB/s (laptop) and 2.4 GB/s (AI). A **23–40× margin** in the
least favourable cell. The earlier estimate of "single-digit MB/s" was right in
spirit and an order of magnitude too generous; the conclusion is unchanged.

**Restore is dominated by a fixed cost, not by blob size.** Gemma on the AI host
takes 417 ms for 130.8 MB and 487 ms for 1833.7 MB — **14× the data for 1.17×
the time**. Apparent throughput therefore *rises* with context, 314 → 3765 MB/s.
The tier gets cheaper per byte exactly as contexts grow, which is the opposite of
the concern that motivated re-deriving its value.

**"Offload doubles restore" does not reproduce, and is retracted.** The earlier
130 → 269 ms observation is contradicted in every pairing here: device restore is
*faster* than host restore at 16k on both models, on both machines (gemma
328 vs 487 ms; qwen 60 vs 77 ms on the AI machine, 171 vs 323 ms on the laptop).
Nothing in the sweep supports sizing the tier around an offload penalty.

### The synthetic control run

Ran `fono-bench coding-client-cache` against a local daemon (laptop, CPU
backend, gemma-4-e2b q4_0, 20480-token context, 4 repo files as tool results,
128-token replies). Three turns took the conversation from 3.9k to 16.7k
estimated tokens; the third prompt tokenised to 19,659, one turn short of the
context ceiling.

The taxonomy from the daemon, one line per lookup:

| turn | prompt tokens | cause | matched | re-decoded prefix |
|---|---|---|---|---|
| 1 | 4,329 | `runtime_key_change` | 0 | 24 |
| 2 | 10,363 | `deepest` | 4,368 | 5 |
| 3 | 19,659 | `deepest` | 10,392 | 5 |

**What this establishes.** The mechanism works end to end over the HTTP
surface: an append-only conversation reuses every token of the previous turn,
and after the cold start only five prefix tokens are ever read twice. Turn 1's
`runtime_key_change` is the empty cache at daemon start, not a failure.

**What it deliberately cannot establish.** Zero evictions and zero divergence
is the expected result for a conversation built to be a perfect prefix, and it
is the answer the gate must not be given by a synthetic. Three entries never
came close to the budget, so nothing was dropped and the `eviction` bucket has
no reading. This run is the control that says a null result from a real client
means something; the gate needs the real client.

**One thing worth its own note.** Reuse does not stop cost growing: time to
first token went 110 s → 211 s → 466 s across the three turns even with 10,392
tokens restored on the last. Prefill is ~50 tok/s on this CPU backend and each
turn adds a whole new file, so the *new* half of the prompt dominates. The
cache removes re-reading; it cannot remove first reading.

**The taxonomy had to reach the log to be readable at all.** A trace file is
started per dictation or assistant turn; a request arriving over the
OpenAI-compatible endpoint starts none, so the first attempt at this run
produced an empty trace directory and no taxonomy. The lookup is now also a
`debug` log line (`crates/fono-assistant/src/llama_local.rs:710-721`), which is
what the table above was read from — `FONO_LOG=info,fono_assistant::llama_local=debug`.
Task 10 needs no trace plumbing, only that filter.

**Repeat run, prewarmed daemon.** Turn 1 reports `deepest` with 23 tokens
matched rather than `runtime_key_change` with none: startup prewarm has already
stored the system-prompt checkpoint. Turns 2 and 3 are unchanged. Only the cold
first row moves, and it moves in the direction that says the pin works.

### The budget is 256 MiB, and it is smaller than one checkpoint

Both embedded backends build their cache with `PromptStateCache::default()` —
**10 entries, 256 MiB** — and nothing in the config can change it
(`crates/fono-assistant/src/llama_local.rs:310`,
`crates/fono-polish/src/llama_local.rs:142`,
`crates/fono-core/src/prompt_cache.rs:493-495`).

Put that beside the blob column measured above:

| Model at 16k | One checkpoint | Against a 256 MiB budget |
|---|---|---|
| gemma-4-26B | 1833.7 MB | **6.8× the whole budget** |
| qwen3.6-35B | 218.2 MB | fits — but only one, once |

An entry larger than the budget is not refused. It is admitted, and the same
enforcement pass that runs on insert drops it again, because eviction walks the
LRU until the total fits and that entry is the only thing in it. Pinned in
`a_checkpoint_larger_than_the_whole_budget_never_survives_its_own_insert`
(`crates/fono-core/src/prompt_cache.rs:1050-1077`).

**So for the model this plan selected, at the context this plan targets, the
cache retains nothing at all.** Every turn is a full cold prefill: 17.9 s on the
device, 194.0 s on the host, against a 328–487 ms restore.

**What this does to the gate — read carefully, because it cuts both ways.** A
real session on gemma today would report `eviction` for nearly every decoded
prefix token, which is nominally the "yes, build Stage C" answer. That reading
would be wrong. The eviction is caused by an arbitrary constant, not by memory
pressure anyone measured, and the cheap fix is to raise the constant, not to
build a storage tier. Only once the budget is set to something defensible does
the eviction/divergence split say anything about disk.

**Therefore Task 10 gains a precondition.** Size the in-memory budget first —
from the machine's actual free RAM and the measured checkpoint size, both of
which Stage A already has — and only then read the taxonomy from a real
session. A tier that pages to disk cannot be justified while the tier in RAM is
capped at a number that predates the measurements.

**How much context 256 MiB actually buys.** The estimator's own pinned per-token
costs (`crates/fono-core/src/gpu_offload.rs:538-563`) make this arithmetic, not
opinion. A `q8_0` row is `1.0625 ×` the dimension against f16's `2 ×`, so a
quantized cache — what this machine runs, per `fono doctor` — costs `0.53125` of
the f16 figure:

| Model | Per token (f16) | Per token (q8_0) | Tokens that fit in 256 MiB |
|---|---|---|---|
| gemma-4-26B | 225,280 B | 119,680 B | **~2,240** |
| qwen3.6-35B | 20,480 B | 10,880 B | ~18,600, less the 65.9 MB fixed state |

So on gemma the whole cache holds **about two thousand tokens** — one system
prompt and part of one file. Not one checkpoint of a working conversation. The
control run only fit three checkpoints because `gemma-4-e2b` is a fraction of
the selected model's size.

The recommendation this points to, for whoever sizes it: derive the budget from
free RAM at load (the machinery already exists — `gpu_offload::decide` measures
free memory rather than trusting the driver) and floor it at a few multiples of
`kv_bytes(n_ctx, …)` for the model actually loaded, so the cache can always hold
at least the current conversation plus one predecessor. A fixed byte count
cannot be right across a 10× range of per-token cost.

**Resolved (Tasks 10a, 10b).** Both halves landed.
`PromptStateCache::sized_for_host` claims a quarter of free RAM past a 4 GiB
desktop reserve, floored at 256 MiB and capped at 8 GiB — the ceiling. Once the
model is loaded and its context known, `budget_prompt_cache`
(`crates/fono-core/src/llama_backend.rs:39`) re-sizes it to
`3 × kv_bytes(n_ctx, type_k, type_v)` under that ceiling — the floor, in the
only unit that survives a 10× range of per-token cost. The cache is still
constructed before the model, which is why the budget is set in two steps
rather than one. Live on the laptop with `gemma-4-e2b` at ctx=20480: 446 MB a
checkpoint, 1338 MB budget. Everything above stands as the record of why the
constant had to go.

---

## Verification Criteria

- One table gives, per matrix cell: prefill ms/token at 16k, KV bytes/token,
  16k blob size, save and restore wall time, cache-directory throughput. Every
  figure measured, none inherited.
- The break-even is stated as an inequality with measured terms on both sides,
  separately per arm, because offload and weight quant move different terms.
- Each asym/Q8_0 pair shows unchanged restore and changed prefill, or the
  discrepancy is explained.
- The Gemma/Qwen contrast yields an explicit statement of how each conclusion
  scales with KV bytes per token.
- The miss taxonomy from a real coding-agent session attributes every wasted
  prefill token to one cause, and the Stage A gate resolves to a documented yes
  or no.
- Proposals 3 and 4 are either killed by Task 9 or promoted with a number.
  Neither remains a theory.
- Flash attention is asserted at load; a forced disable fails the assertion.
- If Stage C ships: a checkpoint written by a **previous process** restores and
  the turn records zero decoded prefix tokens; a corrupted or wrong-version blob
  yields a clean miss and a deleted file, never a crash; the tier refuses a
  memory-backed directory; turning it off leaves no directory;
  `./tests/check.sh --size-budget` still passes.

## Potential Risks and Mitigations

1. **Picking the model on capability alone.** Gemma leads by one task on a
   ten-task, single-seed suite — inside the noise — while costing 11× the KV per
   token, which is the resource this feature spends.
   Mitigation: run both; treat the spread as the sensitivity analysis rather
   than as a contest.
2. **The Q8_0 and asym GGUFs are different base checkpoints.** The Gemma
   filenames do not all carry the `A4B` tag.
   Mitigation: Task 4 verifies before the arm runs; drop the arm rather than
   report a void comparison.
3. **Measuring Fono's own prompts instead of a coding agent's.** The gate would
   answer the wrong question.
   Mitigation: Task 3 drives the HTTP surface the way a client does.
4. **Restore does not stay flat at coding scale.** The standing 14–39 ms figure
   comes from 5–28 MB blobs; a Gemma 16k checkpoint is ~1.9 GB, and offload was
   already seen to double restore.
   Mitigation: Tasks 5 and 7 re-measure; no decision rests on the old number.
5. **Write amplification at 16k**, an order of magnitude worse on Gemma.
   Mitigation: write-back on eviction with content-addressed skip-if-exists, and
   a measured footprint before the feature is defaulted on.
6. **Persisting on top of a broken recurrent rewind** turns transient drift into
   stored drift on the default model.
   Mitigation: Task 25 gates Stage D; Stage C's quarantine deletes any entry
   that produced degenerate output.
7. **Building the tier while the prefix keeps breaking** — shipping all of Stage
   C and observing nothing.
   Mitigation: the Task 10 gate is a precondition, not a review step.
8. **The tier eats RAM on a tmpfs cache directory.** True on a target today.
   Mitigation: Task 8 detects, Task 15 refuses.

## Alternative Approaches

1. **Stage A only, then stop.** If the gate says divergence dominates, publish
   the numbers and build nothing. A legitimate result, not a failure.
2. **Ship Stage C for the HTTP surface alone**, leaving dictation RAM-only.
   Local checkpoints are small, recur only inside a short thread window, and are
   the source of most stale entries; the coding path is where a miss costs
   minutes.
3. **Choose the model to fit the feature rather than the reverse.** If Stage A
   shows Gemma's ~1.9 GB checkpoint is unworkable on a laptop, recommending Qwen
   for the coding surface is cheaper than engineering around 11× the bytes — at
   a cost of one task on a suite too small to resolve it.
4. **Sequence forking instead of a disk tier.** Shares one prefix in one arena
   instead of duplicating it per snapshot. Rejected as the *primary* mechanism
   because it does not survive a restart, which is goal 1 — but it composes, and
   may dominate within a session.
5. **Do nothing and raise the in-RAM budget.** Captures some of goal 2 on a
   128 GB machine, none on a laptop, and nothing of goal 1. The control the
   other options must beat.
