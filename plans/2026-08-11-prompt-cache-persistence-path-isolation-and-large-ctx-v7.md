# Prompt-state cache: disk persistence, path isolation, large-context API

*v7 — supersedes v6. Revised after the "two workloads" challenge: local
dictation (short, ~100 ms budget, dies with the interaction) vs API coding
sessions (long, ~5 s budget, resumes tomorrow).*

## Objective

Make the prompt-state cache survive restarts and serve two workloads whose
sizes and recurrence intervals differ by ~100×, using **one storage layer, one
lookup path, and one eviction policy** — with the workloads separated only
where they provably differ.

---

## Measurements this version rests on

Corrections to earlier versions are marked.

| Quantity | Value | Source |
|---|---|---|
| KV bytes per token | ~18.4 KB | measured, default model |
| Stored blob size | **token-scaled, not `n_ctx`-scaled** | `state.truncate(saved_bytes)`, `crates/fono-assistant/src/llama_local.rs:1954` |
| Save-time allocation | **`n_ctx`-scaled** | `vec![0_u8; ctx.get_state_size()]`, `crates/fono-assistant/src/llama_local.rs:1946-1947` |
| Restore | flat 14–39 ms memcpy | measured |
| Prefill | 8.7–14 ms/token | measured |
| Runtime key includes context size | yes (`ctx={}`) | `crates/fono-assistant/src/llama_local.rs` runtime identity, `crates/fono-polish/src/llama_local.rs` |

**Correction to v5/v6.** v5 costed a local conversation checkpoint at ~151 MB.
That is the *full 8k context* figure. Blobs are truncated to bytes actually
written, so a real local checkpoint at 300–1500 tokens is **5–28 MB** — which
matches the "~28 MiB of dead weight" already noted at
`crates/fono-core/src/prompt_cache.rs:182-183`. v5's "10 threads/day = 13 GB"
arithmetic was inflated ~10×. The real local figure is well under 1 GB/day, and
v6's elaborate retention policy was built on the inflated number.

Derived workload profile:

| | Local dictation | API coding session |
|---|---|---|
| Prompt length | 300–1500 tokens | 8k–60k tokens |
| Blob size | 5–28 MB | 150 MB–1.1 GB |
| Miss cost | ~3–20 s prefill | ~90–600 s prefill |
| Recurrence interval | seconds, then never (5-min thread) | hours to days |
| Restore-from-disk cost | ~30 ms | 0.5 s (NVMe) – 4 s (slow disk) |

---

## The result that lets one policy serve both

Value of an entry = prefill avoided = `tokens × ~11 ms`.
Cost of an entry = bytes stored = `tokens × 18.4 KB`.

**Value per byte stored is constant across both workloads.** A coding
checkpoint is ~40× more valuable and ~40× larger; the ratio cancels. So a
byte-budgeted LRU is *already* the correct global policy — no size-awareness,
no weighting, no cost model. That is the single most important finding in this
version, because it deletes the entire class of "should big entries be treated
differently" machinery.

The two workloads differ on exactly **one** axis that matters: **recurrence
interval**. Everything else — lookup, accounting, eviction, storage — can and
should be identical.

## Correcting the framing in the request

Two points, stated plainly because they change the design:

1. **Case 2 is more latency-critical, not less.** Tolerance is 5 s, but the
   *penalty* for a miss is 90–600 s — 30–100× worse than case 1's. A tolerance
   of 5 s against a miss cost of 300 s means the cache is not an optimisation
   for the API path, it is the difference between usable and unusable.
2. **Case 1 barely needs the disk tier at all.** Within a 5-minute thread every
   reuse is already served from RAM. Disk persistence buys the local path
   *only* survival across a daemon restart that happens mid-thread — rare, and
   already softened by conversation resume. The one local artefact worth
   persisting is the **pinned system prefix**, which is tiny (~5 MB), restored
   on *every* turn, and is what makes the first utterance after a restart fast.

That asymmetry is the design.

---

## The design

Five rules. Nothing else.

1. **One store.** A directory of blob files, filename = hash of the cache key.
   `memmap2` map on read, handed straight to `set_state_data`; save writes
   through a mapped destination file. RAM holds only the index (key,
   `prefix_tokens`, length, mtime) — ~4–5 MB for hundreds of entries. The
   kernel page cache is the hot tier; there is no second RAM tier and the
   256 MiB byte budget is retired, not re-tuned.
2. **Write to disk only what is worth persisting: pins and API checkpoints.**
   Local conversation checkpoints (`HistoryPrefix`, `F8ChatPrefix` from the
   hotkey path) stay RAM-only and die with the process. They are small, they
   recur only inside a 5-minute window, and they are the entire source of the
   "hundreds of stale items" problem.
3. **Retention is LRU against the cap, plus two deletions that are provable
   rather than heuristic:** an entry whose `runtime_sha256` is no longer
   current is unreachable by construction (`crates/fono-core/src/prompt_cache.rs:626-634`)
   and is deleted; and a hygiene backstop deletes anything untouched for 14
   days so an abandoned machine does not hoard transcripts indefinitely.
4. **Two contexts, one model, automatic namespacing.** The API surface gets its
   own `n_ctx`; the runtime identity already includes `ctx=`, so the two
   surfaces land in disjoint key spaces with no owner tag, no reservation
   scheme, and no cross-path invalidation. Path isolation falls out of the
   existing key.
5. **One config key**, absent by default: `prompt_cache_gb: Option<u32>`.
   Absent = automatic (`min(4 GiB, 30% of free disk)`), `0` = disabled.

### Why the elaborate retention policy is gone

At coding scale, 4 GiB holds ~7 checkpoints. **The cap binds long before any
TTL does**, so a tuned TTL is nearly vestigial for the workload it was meant to
protect. And once local checkpoints are not written at all, the workload it was
meant to *contain* no longer exists. v6's 72 h/144 h split, the `reused` bit,
the conversation-window TTL and the thread-close hook are all deleted. What
remains is LRU + stale-runtime deletion + a 14-day hygiene sweep, which is a
timestamp comparison over an in-RAM index.

### Why persisting stale-runtime entries is *not* a nuke

v5's Task 25 deleted the whole directory on a model swap. Under two contexts
that is a defect: the API surface and the local surface have different runtime
keys, so "the current runtime" is plural. Files are grouped by runtime hash and
a group is dropped only when no live engine claims it.

---

## Implementation Plan

### Stage 0 — measure before building (gate)

- [ ] Task 1. Instrument the API surface to log, per request, the
      longest-prefix match length as a fraction of prompt length. This is the
      single number the whole disk tier depends on and it is currently unknown.
- [ ] Task 2. Run a real coding agent against the OpenAI surface for a day
      across at least one restart and one resumed-next-day task. Record match
      fraction, blob sizes and restore times.
- [ ] Task 3. **Gate:** if the median match fraction on resumed sessions is
      below ~50 %, stop and fix prefix stability first (see Risk 1) — the disk
      tier cannot pay for itself against a prefix that shifts.
- [ ] Task 4. Extend the miss taxonomy with a per-surface cause breakdown
      (`cold`, `runtime-changed`, `evicted`, `expired`, `prefix-diverged`), so
      every cold prefill is attributable. Replaces v6's `expired` bucket.

### Stage A — decouple the two surfaces

- [ ] Task 5. Give the API surface its own context size, sized from the
      arriving prompt and capped by an affordability check against available
      RAM. Confirms the runtime key diverges and the two surfaces stop sharing
      a key space.
- [ ] Task 6. Verify one model with two contexts shares mmapped weights, so the
      second context costs KV allocation only, not a second copy of the weights.
- [ ] Task 7. Make pins per-surface, so an API request cannot release the
      locally prewarmed `F8System` pin (`crates/fono-core/src/prompt_cache.rs:533-537`).
      This is the measured 4.5× regression from v2 and it is independent of
      everything else here.
- [ ] Task 8. Decouple `STATE_FORMAT_VERSION` from `CARGO_PKG_VERSION` and
      replace the GGUF mtime in the runtime identity with a content hash, so
      blobs are not invalidated by an unrelated release or a file touch.

### Stage B — disk store

- [ ] Task 9. Confirm `memmap2` is already an edge on every shipped target
      (`cargo tree -p fono -i memmap2`); if net-new on Windows/macOS, hand-roll
      over `libc` / `windows-sys` as `hwcheck`'s `statvfs` already does
      (`crates/fono-core/src/hwcheck.rs:570-593`).
- [ ] Task 10. Blob file format: fixed header (format version, runtime hash,
      token count, wall-clock last-used, `prefix_tokens`) followed by the raw
      state. Wall-clock, not `Instant` — `last_used` is monotonic and cannot be
      persisted (`crates/fono-core/src/prompt_cache.rs:170-174`).
- [ ] Task 11. Save path writes through a mapped destination file via
      `copy_state_data`'s `*mut u8`, eliminating the `n_ctx`-scaled transient
      allocation (~600 MB at 32k) at `crates/fono-assistant/src/llama_local.rs:1946-1947`
      and its duplicate at `crates/fono-polish/src/llama_local.rs:921-932`.
- [ ] Task 12. Restore path maps the file and passes the slice to
      `set_state_data`. No intermediate copy.
- [ ] Task 13. Index rebuild at startup: read headers only, drop files whose
      format version or runtime hash no group claims, drop unreadable files
      silently.
- [ ] Task 14. Persist only pinnable layers and API-surface checkpoints. Local
      conversation checkpoints remain RAM-only.
- [ ] Task 15. Retire the 256 MiB byte budget and the 10-entry cap. Bound
      resident blobs by nothing (the page cache does it); bound stored blobs by
      the disk cap.
- [ ] Task 16. Self-quarantine: a blob that fails to restore deletes itself and
      the turn proceeds as a clean miss.
- [ ] Task 17. Unlink-on-prune, with a deferred-delete queue on Windows where
      unlink-while-mapped is not permitted.

### Stage C — retention

- [ ] Task 18. Derive the cap: `min(4 GiB, 30 % of free disk)` from
      `hwcheck::probe` (`crates/fono-core/src/hwcheck.rs:570-593`), re-derived
      each startup; disable if it cannot hold ~2 checkpoints of the configured
      context.
- [ ] Task 19. LRU eviction against the cap at insert.
- [ ] Task 20. Delete file groups whose runtime hash no live engine claims,
      grouped so a model swap on one surface does not clear the other.
- [ ] Task 21. 14-day untouched sweep at startup, beside the existing history
      purge (`crates/fono/src/session.rs:3563-3584`). Justified as privacy
      hygiene, not performance.
- [ ] Task 22. Add `prompt_cache_gb: Option<u32>` beside `retention_days`
      (`crates/fono-core/src/config.rs:1695-1701`), `skip_serializing_if` so a
      fresh `config.toml` is byte-identical to today's.
- [ ] Task 23. Inherit the opt-out from `[conversations].enabled`
      (`crates/fono-core/src/config.rs:1717-1726`) — declining to store
      conversations must also decline KV blobs derived from them (ADR 0040).

### Stage D — large context

- [ ] Task 24. Raise `n_batch` in step with `n_ctx`; today it caps prompts at
      ~2048 tokens (finding F-J001, `docs/audit/findings.md:3320-3381`), so a
      larger context alone buys nothing.
- [ ] Task 25. Set `type_k` / `type_v` to `q8_0` for the API context — never set
      anywhere in the workspace today, halves KV RAM, disk and write volume.
- [ ] Task 26. Admission control: refuse to cache a blob larger than a fraction
      of the cap, so one giant checkpoint cannot evict the whole store.

### Stage E — surfaces and UX

- [ ] Task 27. Panel and `fono doctor` in plain language: conversations
      remembered, disk used against ceiling, warm or not. Show used-vs-cap so
      the cap never reads as a target.
- [ ] Task 28. "Forget remembered prompts" control; clear-history wipes the
      cache too.
- [ ] Task 29. Fail invisibly — version change, model change, corrupt blob all
      produce a silent clean miss and a debug log. No toast, no startup warning.
- [ ] Task 30. Never fill a disk: reserve headroom, prune early, report only in
      the panel.
- [ ] Task 31. Memoise the engine for `/v1/audio/*`, which rebuilds it per
      request (`crates/fono/src/daemon.rs:4968-4973`, `:4891-4897`) while
      Wyoming and the hotkey reuse a warm one. Unrelated to the KV cache;
      tracked here because it is the larger latency defect on that surface.

### Stage F — only if measurement demands it

- [ ] Task 32. Persist local conversation checkpoints with a TTL equal to the
      conversation resume window (`idle_timeout_minutes`,
      `crates/fono-core/src/config.rs:1717-1735`). Take this only if Task 4
      shows restart-mid-thread cold prefills are common.
- [ ] Task 33. Server-side conversation ids as a retention hint, if the API
      path needs a causal end-of-life signal.

---

## Verification Criteria

- A resumed coding session next day restores in under 5 s where a cold prefill
  would take over 60 s, across a daemon restart.
- The first local utterance after a restart is served from a disk-restored pin,
  with no measurable regression against the current RAM-only warm path.
- A model swap on one surface leaves the other surface's stored blobs intact
  and reachable.
- Steady-state disk use on a dictation-only machine is under ~50 MB — the local
  path writes pins and nothing else.
- Peak RSS during a save at a 32k context shows no `n_ctx`-scaled transient.
- A diff of the serialized default config before and after the whole plan shows
  `prompt_cache_gb` and nothing else.
- Deleting the cache directory while the daemon runs degrades to clean misses,
  never an error shown to the user.

## Potential Risks and Mitigations

1. **API prefix instability makes the hit rate near zero.** Date-stamped system
   prompts, non-deterministic tool ordering, and context compaction that
   rewrites the middle of `messages[]` all break longest-prefix matching at low
   token offsets. This is the risk that could make the whole disk tier
   worthless and it is outside our control.
   *Mitigation:* Stage 0 measures it before anything is built, and Task 3 is a
   hard gate.
2. **Restore from a cold page cache on slow storage.** 1.1 GB at 150 MB/s is
   ~7 s, over the stated budget.
   *Mitigation:* still 40× better than the prefill; Task 25's `q8_0` halves it;
   report observed restore throughput in the panel.
3. **Write amplification.** Every API turn writes a fresh multi-hundred-MB blob.
   *Mitigation:* `prune_dominated_by` (`crates/fono-core/src/prompt_cache.rs:489-522`)
   already collapses a branch to its frontier; measure bytes written per hour
   and, if painful, checkpoint every Nth turn rather than every turn.
4. **Two contexts double KV RAM.** A 32k API context plus an 8k local one is
   ~700 MB resident before any caching.
   *Mitigation:* Task 6 confirms weight sharing; Task 5's affordability check
   refuses a context the machine cannot hold; Task 25 halves it.
5. **Not persisting local checkpoints is wrong.** If restart-mid-thread turns
   out common, the local path loses something it had in v6's design.
   *Mitigation:* Task 4 makes it countable; Stage F Task 32 restores it behind
   one condition.
6. **Privacy.** KV blobs are a materialisation of transcripts, now durable.
   *Mitigation:* Tasks 21, 23, 28. Note this design stores strictly *less*
   conversational content than v6 did, since local transcripts never reach disk.

## Alternative Approaches

1. **Size-scaled TTL** (`TTL ∝ token_count`) instead of class-based
   persistence. More elegant — it self-tunes and needs no surface tag, since
   `token_count` is already in the key. Rejected as harder to explain, harder
   to test, and unnecessary once the cap binds first at coding scale.
2. **Keep v6's 72 h/144 h `reused` split.** Rejected: built on an inflated size
   estimate, and made vestigial by the cap binding first.
3. **One shared context at 32k for both surfaces.** Simpler, but pays 32k KV
   RAM on a dictation-only machine and merges the two key spaces, reintroducing
   the cross-path eviction problem this version deletes.
4. **Persist pins only, nothing else.** The minimal shippable version: no cap,
   no eviction, no config key, a few MB on disk. Delivers the local
   warm-restart win and none of the coding win. A reasonable Stage B.0 if
   Stage 0's gate fails.
