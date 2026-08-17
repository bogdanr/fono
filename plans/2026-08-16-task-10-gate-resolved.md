# Task 10 resolved: the gate says build the disk tier

**Run date:** 2026-08-16. Laptop, Intel Lunar Lake iGPU (Vulkan),
`qwen3.6-35b-a3b-IQ4_XS`, `ctx=20480`, Fono at `3adbc30`.

**Verdict: eviction dominates. Stage C is justified.**

---

## 1. What was run, and why this shape

The earlier agent sessions (see
`plans/2026-08-15-task-10-real-coding-agent-run.md`) proved the mechanism works
but never answered the gate: one conversation, four turns, **zero evictions**.
The gate needs conversations to *compete*, and it needs an earlier conversation
revisited after its checkpoint has been dropped — which no single agent session
can produce, because an agent only ever grows one conversation forward.

So this run drives the same HTTP surface from a script
(`/tmp/fono-gate/gate-workload.py`), with six conversations seeded from six real
repository files at 40 KB each (~9–13k tokens). The client is scripted; the
prompts, the endpoint, the code path and the cache are the real ones.

The budget makes the competition concrete:

```
prompt cache: 637 MB budget, 212 MB a checkpoint at ctx=20480
```

**Three checkpoints fit.** Six conversations were opened, so at least three had
to be dropped. Note this binds on *bytes*, not on `max_entries` — the
hard-coded 10 was never approached, which removes the confound flagged in the
v2 plan.

Rounds:

1. Turn 1 of conversations 1–6, in order.
2. Turn 2 of conversation **6** (newest, expected still cached — a control),
   then turn 2 of **1, 2, 3** (oldest, expected dropped).
3. Daemon restarted, then turn 3 of conversation 6.

## 2. Round 2 — the answer

| Conversation | Cause | Matched | Re-decoded prefix | Recoverable | Wall time |
|---|---|---|---|---|---|
| 6 (newest) | `deepest` | 4,599 | 5 | — | **4.3 s** |
| 1 | **`eviction`** | 22 | 10,257 | 10,274 tok / 178 MB | **75.8 s** |
| 2 | **`eviction`** | 22 | 9,518 | 9,535 tok / 170 MB | **80.4 s** |
| 3 | **`eviction`** | 22 | 12,655 | 12,672 tok / 204 MB | **113.9 s** |

Read the top row against the other three. Same model, same device, same kind of
prompt, same turn number. The one whose checkpoint survived answered in **4.3
seconds**; the three whose checkpoints had been dropped took **76 to 114
seconds** to answer the same shape of question.

That is the gate's question answered directly: **a persisted checkpoint would
have been matched on a later turn.** The tombstones name what it would have
been worth — 10,274, 9,535 and 12,672 decoded prefix tokens, against blobs of
178, 170 and 204 MB.

The 22-token match in the eviction rows is the pinned system prefix, all that
was left once the conversation's own checkpoint was gone.

**Totals for the round:** 32,435 prefix tokens re-decoded because a checkpoint
was dropped, against **5** re-decoded for any other reason. Eviction is not
merely dominant; divergence is absent.

## 3. Round 3 — the case measurement cannot make from inside

The daemon was stopped cleanly and restarted, then conversation 6 — the one that
had answered in 4.3 seconds — was continued:

```
cause="runtime_key_change" matched_tokens=0 decoded_prefix_tokens=4673
```

**125.7 s.** The 4,599 tokens that had been free a few minutes earlier were read
again from nothing, and the in-process taxonomy cannot even call it eviction: the
cache is empty, so there is no tombstone and no cause beyond "different runtime".

This is the part of the argument no amount of in-memory measurement produces.
Every restart — an update, a crash, a reboot, a machine suspended overnight —
throws away every checkpoint, and the loss is invisible to the very diagnostic
built to see it.

One caveat on that number: at restart the previous process had not yet released
its memory, so the offload decision saw 5.9 GB free and ran on the processor.
The 125.7 s is therefore CPU-side and not comparable to the GPU timings above.
The cache result — a total miss after restart — is unaffected.

## 4. An unrelated finding worth recording

Turn 1 of every conversation showed the same thing: prefix 27 tokens, the whole
10k file re-decoded as suffix. That is correct, not a defect — `split_messages`
puts the system prompt and prior history in the prefix and the trailing turn in
the suffix, and on turn 1 there is no history. It also explains why only one
lookup was logged in round 1: conversations 2–6 hit the 27-token system prefix
on its exact key, and the lookup taxonomy only runs on an exact-key miss.

Consequence for reading any future trace: **turn 1 of a conversation is invisible
to the taxonomy.** The counts above are turn-2 counts, which is the right place
to look anyway.

## 5. What this settles, and what it does not

**Settled — Stage C is justified.** The condition the v2 plan set was "a
persisted checkpoint would have been matched on a later turn". Three out of
three revisited conversations, 32,435 tokens, 70–110 seconds each.

**Settled — the byte budget binds first.** At 212 MB a checkpoint against a
637 MB budget, three conversations is the ceiling on this machine. `max_entries
= 10` never came into it. Raising a constant is not an alternative to storage
here: the budget is already a quarter of free RAM, and the honest way to hold
more conversations is to stop holding them all in memory.

**Settled — the economics still hold.** The disk tier has to restore ~200 MB to
save 70–110 seconds of prefill. Measured cold storage on this machine is
7.3–8.2 GB/s, so the read is ~25 ms.

**Not settled — retention policy.** Six conversations at ~200 MB each is 1.2 GB
after one short session. How much disk, for how long, and what to drop first is
Task 22's question and this run does not answer it. The v7 proposal of "30 % of
free disk, capped at 4 GiB" was written before anyone knew a checkpoint could be
200 MB — under that cap this session alone would fill a fifth of it.

**Not settled — the 13.4 GB/s storage figure in the v2 plan** is almost
certainly page cache, not disk. It does not change the conclusion (the margin is
70× rather than 129×) but it is a recorded measurement that is wrong, and the
`fono-bench` storage probe that produced it measures the wrong thing.

## 6. Cleanup

Daemon stopped, model symlink removed, port free, no accelerator marker left
behind. Nothing in the repository was changed by the run itself.
