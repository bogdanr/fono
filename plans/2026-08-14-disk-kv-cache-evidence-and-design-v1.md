# Disk KV Cache: Prove What Works, Then Build It

*Superseded by `plans/2026-08-14-disk-kv-cache-evidence-and-design-v2.md`, which
carries the model selection, the execution recipe and the measurements. This
version is kept only for the long reasoning behind the six proposals.*

*Supersedes the measurement half of `plans/2026-08-01-prompt-state-cache-disk-tier-v1.md`
and re-derives `plans/2026-08-11-...-v7.md` against two stated goals rather than
against the dictation workload it was written for.*

## Objective

Decide, from measurement rather than theory, what the disk-backed KV cache must
be in order to serve two goals:

1. **Fast restore across a Fono restart.**
2. **Make a local coding agent feasible on machines that cannot otherwise run
   one** — target working context **16k tokens**.

Four measurement arms: the AI machine (Ryzen AI MAX+ 395, 128 GB, 96 GiB
firmware carve-out) and the laptop, each on CPU and on GPU.

The deliverable of Stage A is a **go/no-go with numbers**, not code.

---

## Assessment of the six proposals

Each is scored against what the codebase already does. Three are already true,
one is out of scope, one is the actual work, one is unproven and expensive.

### 1. Single-Active-Slot KV Cache Swapper — **already the architecture**

Fono holds exactly **one context per role**, and every batch hardcodes sequence
`0`. There is no scheme anywhere that keeps 2–3 project contexts resident. What
*is* resident is a set of serialized snapshots in `PromptStateCache`
(`crates/fono-core/src/prompt_cache.rs`), byte-budgeted and LRU'd — blobs, not
contexts.

So "single active slot + swap state in" is a description of what exists plus a
disk tier behind it. This proposal collapses into the disk tier itself. Nothing
separate to build.

**One real gap it does surface:** the snapshot budget is RAM-only, and a single
16k coding checkpoint will exceed the whole default byte budget. That is the
argument for the disk tier, and it is sizing, not architecture.

### 2. Radix tree + `kv_cache_seq_cp` fork — **two different ideas, split them**

**2a. Radix index over `prefix_tokens`.** Replaces the linear scan in
`find_longest_prefix`. Prior measurement: at 500 entries × 2000 tokens the
linear scan costs about a millisecond, against a miss costing minutes. This is
a nice-to-have. **Do not build it until an entry-count measurement says the scan
is visible.**

**2b. Fork sequence 0 with `kv_cache_seq_cp`.** This is real, and it is the
highest-leverage idea in the list after the disk tier. The bindings expose
everything needed:

- `LlamaContextParams::with_n_seq_max` (`context/params/get_set.rs:113`)
- `kv_cache_seq_cp` (`context/kv_cache.rs:137`)
- `state_seq_get` / `state_seq_set` with flags (`context/session.rs:582,624`)

Today Fono uses **none** of it. The win is that a shared immutable prefix is
held **once** in the KV arena and forked per turn in-arena, instead of every
snapshot embedding its own copy of the head. It also converts a whole-state
memcpy into a range copy.

Two hard constraints, both must be measured, not assumed:

- All live sequences share one `n_ctx`. Sum of live tokens across heads must
  fit. At 16k per head on a laptop this may allow exactly one head.
- The binding documents `PARTIAL_ONLY` as the **only** correct rewind for Gated
  Delta Net models — which is what `gemma-4` is. Fono's post-generation
  truncation currently uses `clear_kv_cache_seq`, which cannot roll back partial
  recurrent state. **This is a latent correctness defect independent of the
  cache**, and it must be settled before any completed-turn checkpoint is
  persisted, or a wrong state becomes a sticky wrong state.

### 3. `madvise` hints on MoE expert weights — **not exposed; unproven; rank last**

llama.cpp owns the `mmap`. Neither `llama-cpp-2` nor ggml's public surface
exposes per-tensor `madvise`. Achieving it means either an upstream patch or
walking `/proc/self/maps` and applying `madvise` to ranges derived from GGUF
tensor offsets — which Fono *can* compute (it already parses GGUF headers in
`crates/fono-core/src/gpu_offload.rs`), but that is a research project with a
maintenance tail.

More importantly the theory only bites when the model is **paging from disk**,
i.e. does not fit RAM. Before writing any of it, falsify cheaply: measure tok/s
with the model resident vs. cold, and measure major-fault counts during
generation. If faults are not a material share of the token budget, the idea is
dead and costs nothing.

### 4. Pin non-expert layers — **same category as 3**

`use_mlock` is all-or-nothing, and `crates/fono-core/src/llama_backend.rs:255`
asserts it stays **off** on purpose: pinning a model larger than RAM is an OOM.
Per-tensor pinning is not exposed. Same `/proc/self/maps` route, same research
tail, same falsification test as 3 — they share one measurement.

### 5. `flash_attn = true` — **already on; nothing to do**

Two facts from the vendored llama.cpp:

- `llama_context_default_params` sets
  `flash_attn_type = LLAMA_FLASH_ATTN_TYPE_AUTO`
  (`src/llama-context.cpp:3493`).
- A **quantized V cache requires flash attention** — context creation returns
  `nullptr` otherwise (`src/llama-context.cpp:3578`).

Fono runs `q8_0` K/V and loads successfully, therefore flash attention is
already enabled. The only defensible action is to **assert** it at load so a
future upstream default flip surfaces as a failure rather than a silent
slowdown. This proposal is otherwise closed.

### 6. Context window pruning with rolling summary — **out of scope for goal 2**

Split it:

- **Capping the window** is sound and nearly free.
- **Rolling summary eviction is actively hostile to caching.** Re-summarising
  rewrites the *front* of the prompt, which invalidates every cached prefix at
  once — exactly the divergence failure previously identified as the dominant
  cause of misses. A cache and a rolling summary at the front of the prompt are
  in direct opposition.
- **For the coding-agent path Fono does not own the context.** The client sends
  the full conversation over the HTTP surface. Fono cannot prune it without
  lying to the client about what it saw. This applies only to Fono's own
  assistant path.

Reframe as: cap `n_ctx`, keep any summarisation strictly *behind* stable
material, and let coding clients manage their own window.

---

## The one thing that decides everything

Every economic argument for this feature already checks out — break-even disk
bandwidth is single-digit MB/s against prefill costing minutes. The read side
has never been in doubt.

**The open question is whether a real coding agent's prompt prefix is stable
turn to turn.** If the client injects a file tree, a timestamp, a token budget
or a shuffled tool list at the front of every request, the deepest match is
near zero and a disk tier stores checkpoints that are never read again.

That is measurable in an afternoon and it gates the whole build.

---

## Implementation Plan

### Stage A — Instrument and measure (no feature code)

- [ ] Task 1. Assert flash attention is enabled at context creation, and record
      the effective policy in the doctor line. Rationale: closes proposal 5 with
      a guard instead of a change, and makes a future upstream default flip
      loud.
- [ ] Task 2. Extend the existing per-lookup cache trace into a **miss
      taxonomy**: for every lookup record the deepest matched token position and
      the reason it was not deeper — capacity eviction, prefix divergence (with
      the divergence token index and the prompt segment it lands in), or
      runtime-key change. Count **decoded prefix tokens**, never hit counts.
      Rationale: this is the gate in Task 8 and it cannot be inferred later.
- [ ] Task 3. Add a `fono-bench` mode that drives the HTTP surface as a coding
      client would: a growing conversation carrying tool results and file
      contents, to 16k tokens. Reuse `AssistantConversationCache` rather than
      writing a new harness. Rationale: the existing harness measures Fono's own
      prompts, which are not the workload under test.
- [ ] Task 4. Measure, on all four arms (AI machine × {CPU, GPU}, laptop ×
      {CPU, GPU}), with a model that is known-good on the device — **not**
      DeepSeek-V4-Flash, whose GPU output is broken by the vendored llama.cpp's
      age:
      - prefill ms/token at 1k, 4k, 8k, 16k
      - KV bytes/token, measured, cross-checked against the estimator already in
        `crates/fono-core/src/gpu_offload.rs`
      - blob size for a 16k checkpoint
      - `state_get` (save) and `set_state_data` (restore) wall time at that size
      Rationale: the standing "restore is a flat 14–39 ms memcpy" figure was
      taken at 5–28 MB and will not survive a 16k coding checkpoint. Every later
      decision rests on the corrected number.
- [ ] Task 5. Measure restore cost **with and without GPU offload** at 16k.
      Rationale: offload was already measured to *double* restore on the 26B
      model (130 → 269 ms) while cutting prefill only 1.29×. The disk tier's
      value is squeezed from both ends and the sign of that trade at coding
      scale is unknown.
- [ ] Task 6. Measure the storage path itself: sequential read and write
      throughput on the resolved cache directory on each machine, and detect
      whether that directory is **memory-backed**. Rationale: `/root` is tmpfs
      on at least one target, where a naive disk tier consumes RAM while
      believing it used the SSD.
- [ ] Task 7. Falsification test for proposals 3 and 4: during generation with a
      model that does **not** fit RAM, record major fault counts and tok/s, and
      compare against the same model resident. Rationale: if paging is not a
      material share of the token budget, both proposals are dead for free.
- [ ] Task 8. **Gate.** Read Task 2's taxonomy from a real coding-agent session.
      Proceed to Stage C only if the prefix is stable enough that a persisted
      checkpoint would have been *matched* on a later turn. If divergence
      dominates, go to Stage B and stop.

### Stage B — Only if divergence dominates

- [ ] Task 9. Identify which prompt segment the divergence lands in, from Task
      2's segment attribution, and order the prompt stable-material-first.
      Rationale: ordering is free and converts a total invalidation into a
      partial one.
- [ ] Task 10. Give the HTTP surface a warm path. It has the most conversations
      and the least cache help; pinning currently requires empty history, so a
      client on turn 5 never pins.
- [ ] Task 11. Re-run Task 8's gate. A disk tier is worth building only against
      a prefix that survives.

### Stage C — The disk tier, sized by Stage A

- [ ] Task 12. ADR before code, matching the posture set by ADR 0040: 0600
      permissions, finite retention, an opt-out that creates **no file at all**,
      and an explicit delete control. A KV blob is a materialisation of the
      transcript.
- [ ] Task 13. Refuse to enable the tier when the cache directory is
      memory-backed; report the refusal in `fono doctor` rather than silently
      consuming RAM.
- [ ] Task 14. Key the persistent entries on an explicit `STATE_FORMAT_VERSION`
      composed with the binding version, and on the **GGUF content hash** rather
      than model mtime. Decouple from `CARGO_PKG_VERSION`. Rationale: as written
      today every Fono point release would discard the entire disk cache, which
      defeats goal 1 entirely.
- [ ] Task 15. On-disk format: header with magic, format version, runtime key,
      token count, payload length and a payload checksum; then the payload; then
      the `prefix_tokens` vector. The token vector is **mandatory** — an entry
      that loses it drops out of longest-prefix matching entirely.
- [ ] Task 16. One content-addressed file per entry under the cache directory,
      published `.part` → verify → rename, mode 0600.
- [ ] Task 17. Write-back on eviction, never write-through on insert, and skip
      the write when the content-addressed file already exists. Rationale: at
      16k the write amplification is the dominant cost of the feature; most
      entries are superseded within the same conversation and must never reach
      the disk.
- [ ] Task 18. All disk I/O off the model mutex — reads resolved before the lock
      is taken, writes on a niced background thread. A multi-hundred-megabyte
      write under the model lock stalls every other conversation.
- [ ] Task 19. Quarantine on failure: any header, checksum or `set_state_data`
      rejection deletes the file and records a miss. Never retry a bad blob.
      Wire the degenerate-output guard to delete the backing entry, or a
      poisoned checkpoint survives restarts.
- [ ] Task 20. Retention: LRU against a byte cap, plus deletion of entries whose
      runtime key is no longer current, plus a hygiene sweep for anything
      untouched beyond a fixed age. Swept at startup alongside the existing
      history purge, not on a timer.
- [ ] Task 21. One config key, absent by default, with `0` meaning disabled.
      A test must fail the build if a second knob appears.
- [ ] Task 22. Confirm no net-new dependency, and **add no compression crate** —
      KV data compresses poorly and the binary size budget is the standing
      constraint.

### Stage D — Sequence forking, gated on a correctness fix

- [ ] Task 23. Settle the recurrent-state rewind first. Establish whether
      `clear_kv_cache_seq` actually corrupts partial state on the default model,
      and if so replace the post-generation truncation with
      `state_seq_get`/`state_seq_set` under `PARTIAL_ONLY`. Rationale: this is a
      correctness defect in its own right, and persisting checkpoints on top of
      a broken rewind turns transient drift into stored drift.
- [ ] Task 24. Measure `kv_cache_seq_cp` fork cost against a full-state restore
      at 16k, and measure how many 16k heads fit inside one `n_ctx` on each arm.
      Rationale: decides whether shared-prefix forking is a real capability on
      target hardware or only on the AI machine.
- [ ] Task 25. Adopt sequence forking only if Task 24 shows both a materially
      cheaper fork and room for more than one head.

### Stage E — Deferred, explicitly

- [ ] Task 26. Radix index over `prefix_tokens`, only once an entry-count
      measurement shows the linear scan is visible against the miss cost.
- [ ] Task 27. Per-tensor `madvise` / partial `mlock`, only if Task 7 shows
      paging is a material share of the token budget. Treat as an upstream
      contribution rather than a local patch.

---

## Verification Criteria

- A single table gives, for all four arms: prefill ms/token at 16k, KV
  bytes/token, 16k blob size, save and restore wall time, and cache-directory
  read/write throughput. Every figure is measured, none inherited.
- The break-even is stated as an inequality with measured terms on both sides,
  separately for CPU and GPU arms, because offload moves both.
- The miss taxonomy from a real coding-agent session attributes every wasted
  prefill token to exactly one cause, and the Stage A gate resolves to a
  documented yes or no.
- Proposals 3 and 4 are either killed by Task 7's fault measurement or promoted
  with a number attached. Neither remains a theory.
- Flash attention is asserted at load; a forced disable fails the assertion.
- If Stage C ships: a checkpoint written by a **previous process** restores and
  the turn records zero decoded prefix tokens; a corrupted or wrong-version blob
  yields a clean miss and a deleted file, never a crash; the tier refuses to
  operate on a memory-backed directory; turning it off leaves no directory;
  `./tests/check.sh --size-budget` still passes.

## Potential Risks and Mitigations

1. **The measurement is run against Fono's own prompts rather than a coding
   agent's.** The whole gate would then answer the wrong question.
   Mitigation: Task 3 drives the HTTP surface the way a client does, including
   tool results and file contents.
2. **Restore does not stay flat at coding scale.** The standing 14–39 ms figure
   is from 5–28 MB blobs; a 16k checkpoint may be an order of magnitude larger,
   and offload was already observed to double restore.
   Mitigation: Tasks 4 and 5 re-measure rather than extrapolate; no design
   decision is taken on the old number.
3. **The model under test misleads.** DeepSeek-V4-Flash produces broken output
   on the GPU with the vendored llama.cpp, and one test model is too small to
   resolve differences.
   Mitigation: Task 4 pins a model known-good on the device on each arm, and
   names it in the results.
4. **Write amplification at 16k.** Checkpoints are large and frequent.
   Mitigation: write-back on eviction with content-addressed skip-if-exists, and
   a measured footprint before the feature is defaulted on.
5. **Persisting on top of a broken recurrent rewind.** Turns transient drift
   into stored drift on the default model.
   Mitigation: Task 23 gates Stage D, and Stage C's quarantine deletes any
   entry that produced degenerate output.
6. **Building the tier while the prefix keeps breaking.** The largest risk:
   shipping all of Stage C and observing nothing, because misses were never
   about capacity.
   Mitigation: the Task 8 gate is the precondition for Stage C, not a review
   step after it.
7. **The tier eats RAM on a tmpfs cache directory.** True on at least one
   target machine today.
   Mitigation: Task 6 detects it, Task 13 refuses.

## Alternative Approaches

1. **Stage A only, then stop.** If the gate says divergence dominates, the
   honest outcome is that no disk tier is built and the numbers are published.
   This is a legitimate result, not a failure.
2. **Ship Stage C for the HTTP surface alone**, leaving the dictation path
   RAM-only. Local checkpoints are small, recur only inside a short thread
   window, and are the source of most stale entries; the coding path is where a
   miss costs minutes.
3. **Sequence forking instead of a disk tier.** Shares one prefix in one arena
   rather than duplicating it per snapshot. Rejected as the *primary* mechanism
   because it does not survive a restart, which is goal 1 — but it composes with
   the disk tier and may dominate within a session.
4. **Do nothing and raise the in-RAM budget.** Captures some of goal 2 on a
   machine with 128 GB and none of it on a laptop, and nothing at all of goal 1.
   Cheapest possible baseline, and the control the other options must beat.
