# ADR 0042 — Prompt checkpoints on disk

- **Status:** Accepted
- **Date:** 2026-08-17
- **Related:** [ADR 0040 — Assistant conversation persistence](0040-assistant-conversation-persistence.md)

## Context

Fono keeps a bounded in-memory cache of prompt-state checkpoints: a serialized
copy of llama.cpp's KV cache, keyed on the tokens it was built from, so a turn
whose prompt starts with a cached prefix restores it and reads only the
remainder. The cache holds a small multiple of one checkpoint — three, budgeted
from free RAM — because one checkpoint of a mixture model at a 20k context is
about 200 MB and of a dense 26B model about 2.3 GB.

Three is not enough for how a coding client works. Measured on this machine
across six conversations driven through the OpenAI-compatible endpoint:

- Returning to a conversation whose checkpoint had been dropped cost **70–114
  seconds** of re-reading. The same question, asked of the conversation still
  held, came back in **4.3 seconds**.
- Across the session, **32,435 prefix tokens** were read a second time because
  a checkpoint had been dropped, against **5** for every other cause combined.
- A daemon restart threw everything away and cost the same again, on every
  conversation.

Reading a checkpoint back from this machine's storage takes **170–218 ms** for
the mixture model and about a second for the dense one. Against 70–114 seconds
of re-reading, the ratio is not close.

That ratio also settles what the in-memory tier is *for*. Holding a checkpoint
in RAM rather than on disk saves only the restore, so it is a 0.2 % improvement
on the thing it optimises — bought with 637 MB of RAM on this machine, and up to
8 GiB on a larger one. The same RAM given back to the page cache is worth far
more: an identical turn took **617 s cold against 47 s warm** on a 16.9 GB
mixture model, because the weights were paging in from storage. So the second
reason for this tier is not speed of resume at all — it is that memory spent
holding checkpoints is memory taken from the model.

Raising the memory budget is not the alternative. It is already a quarter of
free RAM, and the constraint is what a desktop can spare, not a constant
somebody picked.

## Decision

### 1. A second tier, on disk, behind the in-memory cache

The in-memory cache stays authoritative and unchanged in shape. Disk is
consulted only when memory has nothing deeper to offer, and answers with the
same longest-prefix rule.

The tier lives under the **cache** directory (`$XDG_CACHE_HOME/fono/`, or
`%LOCALAPPDATA%\fono\cache` on Windows), not the data directory. A checkpoint is
derived, disposable and re-computable: deleting it costs time and nothing else,
which is exactly the guarantee the cache directory carries. It must not be
swept into a backup of the user's data.

### 2. Same privacy posture as conversation history

A checkpoint is a materialisation of the prompt that produced it. Restoring one
into a model reproduces the conversation's context as faithfully as replaying
the transcript, so it inherits ADR 0040's posture rather than being treated as
an opaque blob:

- **Mode `0600`** on Unix, on every file, set at creation.
- **Finite retention** — a byte cap, deletion of checkpoints whose runtime key
  is no longer current, and a hygiene sweep of anything untouched for 14 days.
  Swept at startup, not on a timer.

  The age sweep earns its place on privacy grounds, not on space: the byte cap
  already bounds the directory, so deleting an idle checkpoint frees nothing
  anybody was waiting for. What it does is stop a copy of a conversation
  outliving any use for it. A hit rewrites the file's last-used timestamp in
  place — eight bytes, not the payload beside them — so anything still in use
  never ages out however old the file is. That covers the prefixes a caller
  sends on every request, which are touched constantly and therefore permanent
  in practice without needing to be marked as such.
- **An opt-out that creates no file at all.** Disabled means the directory is
  never created. The absence of the directory is the only credible proof.
- **An explicit delete control**, so a user can clear the tier without knowing
  where it lives.

The stored payload is opaque model state, so there is nothing to redact. The
protections above are what carry the posture, exactly as ADR 0040 concluded for
conversation turns.

### 3. Refuse a memory-backed directory

If the cache directory is on `tmpfs` or `ramfs`, the tier is not enabled: a
"disk" tier in RAM spends the same memory the budget was trying to protect,
twice, and reports a saving it did not make. The refusal is reported in
`fono doctor` rather than being silent, because the user's cache directory being
in RAM is a fact they may want to act on.

A user whose home directory is in RAM — a live system, or a deliberately
RAM-only setup — therefore gets no disk tier. That is the correct answer, not a
limitation: both reasons the tier exists fail there. Files do not survive the
reboot, so resume is no faster; and the bytes come out of the same pool the tier
is meant to hand back to the model, so it makes memory pressure worse while
reporting an improvement. There is no override, because an override would only
let a user ask for a slower Fono. Pointing the cache directory at real storage
is the fix, and the message says so.

The check reads `/proc/mounts` and takes the longest mount point that prefixes
the path. `statfs` would answer in one syscall, but only by hand-declaring a
struct whose layout differs between libcs, and the question is asked once per
model load.

### 4. Keyed on content, not on timestamps

A stored checkpoint is only valid for the exact runtime that produced it. The
key composes:

- an explicit **state format version**, bumped by hand when the on-disk layout
  or the meaning of the payload changes;
- the **llama.cpp binding version**, because the payload is its serialization
  format and it is not promised to be stable across versions;
- the **model's content identity**, plus the context length, cache types and
  attention settings that change the state's shape.

It deliberately does **not** compose Fono's own package version. Keying on that
would discard every checkpoint on every point release, which defeats the purpose
the measurement established.

### 5. Two write triggers: eviction and clean shutdown

A checkpoint is written when the in-memory cache drops it, and when the process
exits cleanly. It is never written on insert: most checkpoints are superseded
within seconds by a longer one from the same conversation, so writing every one
would spend 200 MB of I/O per turn on something already dead. Eviction stores
exactly what the measurement showed is worth storing; shutdown covers the case
no in-process measurement could see, because an empty cache leaves no trace of
what it used to hold.

There is no third trigger. Periodic waypoints part-way through a growing
conversation are a plausible idea with nothing measured behind them, and each
one costs a full write.

Note the coupling: the write rate is set by how many checkpoints memory holds.
A one-deep memory tier writes on every conversation switch. At the measured cost
below that is acceptable, but shrinking the memory tier further is not free and
must not be done as a stray optimisation.

### 6. Writes are synchronous, and nothing is fsynced

Measured on the laptop's NVMe (2026-08-17): a 212 MB checkpoint blocks the
caller for **0.06 s** and a 2,337 MB one for **0.63 s**, both about 3.5–3.7 GB/s.
That is page-cache bandwidth, not disk bandwidth — the same drive reads cold at
7.3–8.2 GB/s and writes nowhere near 3.7. `write()` returns once the bytes are
in the page cache; the kernel's writeback threads move them to the platter
afterwards. So the answer to "doesn't it flush in the background, like `cp`?" is
yes, and precisely *because* there is no `fsync` (see below).

Against a turn of 4 to 113 seconds, 0.06–0.63 s is 0.5 % to 1.5 %, so the write
happens inline where it is easy to reason about. If it ever shows up in a
measurement, moving it off the model lock is a change to make then, with the
number in hand.

One caveat that would change this: if dirty pages exceed the kernel's
`dirty_ratio`, `write()` starts throttling and the cost becomes real disk
bandwidth. A 2.3 GB checkpoint on a machine already writing heavily could
therefore block for seconds rather than tenths. It has not been observed, and
the fix if it is would be the background thread above, not an `fsync`.

No `fsync`, anywhere, including at shutdown. Publishing by `rename` already
survives a process abort, a clean exit and a reboot, because the page cache
outlives the process and the shutdown sequence syncs filesystems. The only
failure `fsync` would additionally cover is a power cut, whose cost is one
checkpoint and therefore one cold prefill — and a truncated file is *detected*
by the validation below rather than mistaken for good data. Against that, an
`fsync` stalls every write and on ext4 can drag unrelated dirty pages with it.

### 7. The memory tier shrinks to a one-deep working set

Once disk holds the rest, the memory budget's multiplier drops from three
checkpoints to one. The conversation being served has its KV resident in the
live context already — that is not a cache copy — so the memory tier's entire
value is *other* conversations, and other conversations are what a 200 ms read
handles. The RAM this returns is the second reason the tier exists.

The budget is sized from the **worst case**: one checkpoint at a full context
window, not at the length conversations happen to reach. A cache that cannot
admit the checkpoint in front of it is worse than one that holds fewer entries.
This wastes 40–60 % of the reservation when conversations are short, and that is
accepted deliberately rather than by accident.

### 8. A bad checkpoint is deleted, never retried

Any failure — a short read, a header that does not parse, a checksum mismatch,
a rejection from llama.cpp when the state is restored — deletes the file and
records a miss. A checkpoint that cannot be restored is worthless, and one that
restores into a broken state is worse than worthless; either way there is
nothing to gain by keeping it for the next attempt.

Validation is the one place care is spent, because a state that did not match
its claimed token count is a bug this project has already shipped once. Every
field is checked on read: magic, format version, runtime key, and file length
against the header's payload length.

### 9. One file per checkpoint, content-addressed, with no index

The file is named by a hash of the runtime key and the token sequence, so a
second write of the same content is a no-op and there is nothing to reconcile
after a crash. Metadata lives in each file's header; lookup scans the directory
and reads headers only. At the single-digit entry counts a real budget permits,
that is microseconds — and it cannot fall out of sync with an index, because
there is no index.

Publishing is stage to a temporary name, then `rename`. A failure unlinks the
temporary file.

### 10. The automatic size has an absolute ceiling

The size is derived — eight checkpoints, capped at a fifth of free disk — but a
derived number still needs a ceiling, because checkpoint size varies by an order
of magnitude between models. Eight checkpoints of a mixture model is 1.7 GiB;
eight of a dense 26B at a 20k context is **18 GiB**, which no user asked for. A
cache should not be the largest thing Fono puts on a disk.

So the automatic size is capped at **4 GiB**. Under the cap a large dense model
gets one or two checkpoints, which is enough for the case that motivated the
tier: resuming the conversation in progress after a restart. A user who wants
more sets the one configuration key, which is not capped — an explicit number is
an instruction, not an estimate.

This is a ceiling on a derived number, not a budget in its own right. The
distinction matters: a fixed 4 GiB budget would hold fifteen checkpoints of one
model and one of another, which is the mistake the derivation exists to avoid.

So the worst case a user should expect to find on disk is **4 GiB**, and only
when free disk allows it and checkpoints are large enough to reach it. For a
mixture model at a 20k context the automatic size settles at about **1.7 GiB**.

### 11. One configuration key

A single key, absent by default, sizing the tier; `0` disables it. Not a family
of knobs for path, count, age and format. A test fails the build if a second one
appears, because "one key" is a decision that erodes silently.

## Consequences

**Positive**

- Returning to an earlier conversation costs a read rather than a re-read: tens
  of milliseconds instead of a minute or more.
- A restart no longer discards everything. This is the case no in-process
  measurement could see, because an empty cache leaves no trace of what it
  used to hold.
- The memory budget stops being the only thing between a user and a cold
  prefill, so it can stay conservative.

**Negative / accepted**

- Fono now writes a materialisation of conversation context to disk where it
  previously kept it only in memory. Mitigated by `0600`, finite retention, a
  directory-creating opt-out and an explicit delete control — and by living in
  the cache directory, where deletion is always safe.
- Disk space. Bounded by the byte cap, and the cache directory is already where
  Fono keeps model weights measured in gigabytes.
- Write amplification on eviction. Bounded by writing only what is evicted and
  only once per distinct content.

## Alternatives considered

- **Raise the memory budget instead.** It is already a quarter of free RAM. The
  measurement's eviction pressure came from six conversations, which is a
  normal working day, not an extreme.
- **Write through on every insert.** Simpler to reason about, and spends most of
  its I/O on checkpoints that are superseded before anything could read them.
- **Compress the payload.** KV state compresses poorly, and a compression crate
  is a new dependency against a binary-size budget that is the standing
  constraint. Explicitly rejected.
- **Store checkpoints in SQLite beside the conversations.** Multi-hundred-megabyte
  blobs in a database that also serves interactive queries, with a retention
  policy that has nothing in common with the transcripts'. One file per
  checkpoint makes deletion, quarantine and the byte cap trivial.
- **ds4's value-per-byte eviction score** (`(hits+1) × tokens / bytes`, with
  exponentially decayed hits). Its discriminating power comes from variance in
  file size; ours vary by 11 % because a checkpoint is a near-constant ~17 KB per
  token. On our data the formula reduces to a decayed hit count, which with most
  entries at zero hits is least-recently-used with extra steps — a decay
  constant and two tuning factors to reproduce a sort. The same reasoning
  declines its anchor-vs-waypoint weighting and superseded-waypoint suppression:
  both exist to rank waypoints, and we write none.
- **ds4's boundary trim and alignment before storing.** The trim guards against
  tokenizer re-merges, which is a hazard only because ds4 keys on rendered
  bytes; we key on tokens. The alignment keeps its KV compressor's row
  finalisation identical, and we have no compressor.
- **Keying on rendered prompt bytes rather than token ids.** Immune to tokenizer
  re-merges at the prefix boundary, at the cost of carrying text alongside the
  payload and reconstructing the effective prompt on read. It would not have
  helped the single divergence the measurement saw, which was a genuinely
  different system prompt. Worth revisiting only if boundary re-merges are ever
  observed.
- **A fixed default byte cap**, as ds4 uses (4 GiB). The same constant means one
  full-window checkpoint of the dense model, three of the sizes measured here,
  or fifteen of the mixture model's — so it has to be derived from checkpoint
  size, exactly as the memory budget already is.
- **Fork llama.cpp sequences instead of serializing state.** A cheaper copy, but
  every fork lives inside one context's token budget, so it does not survive a
  restart and does not scale past a couple of conversations. Kept as a separate
  question, gated on a correctness fix for recurrent-state rewind.
