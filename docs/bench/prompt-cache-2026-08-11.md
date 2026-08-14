# Prompt-state (KV) cache measurements — 2026-08-11

Baseline for the prompt-state cache work. Every number here was produced by
`fono-bench assistant-prefix-cache`, which prefills a prefix, saves the state,
restores it into a fresh context, prefills only the suffix, and generates from
both arms in the same process against the same loaded model — so the only
difference between the two arms is the cache.

Re-run the same commands after the disk tier lands and diff against this file.

## Host

| | |
|---|---|
| CPU | Intel Core Ultra 7 258V, 8 cores / 8 threads |
| RAM | 30 GB total, ~20 GB available |
| SSD | Samsung MZVLC1T0HFLU, 954 GB, NVMe |
| Kernel | 7.0.0 |
| Fono | `24dcb22` + the changes described below |
| Priority | normal (never `nice`d — a niced measurement competes wrongly for cores) |

Models, both local, no download:

| File | Size | Quant |
|---|---|---|
| `gemma-4-26B_q4_0-it.gguf` | 14.4 GB | uniform 4-bit |
| `gemma-4-26B-asym-v15-gu.iq2xxs-dn.q2k-pad.gguf` | 9.60 GB | asymmetric 2-bit |
| `qwen3.6-35b-a3b-asym-guiq2xxs-dnq2k.gguf` | 11.7 GB | asymmetric 2-bit |
| `DeepSeek-V4-Flash-0731-UD-IQ3_XXS-*-of-00004.gguf` | 104 GB, 4 shards | unsloth dynamic 3-bit |

Benchmark subjects only. `gemma-4` is published under the `gemma` licence,
which is not OSI-approved, so it cannot become a Fono default
(see `docs/decisions/0004-default-models.md`).

## What a checkpoint costs

`state_bytes` is the size of the saved blob, truncated to bytes actually
written rather than the allocated upper bound.

| Model | Prefix tokens | Saved state | Per token |
|---|---|---|---|
| gemma-4-26B q4_0 | 1,050 | 226 MiB | **220.0 KB** |
| gemma-4-26B q4_0 | 6,685 | 1,436 MiB | **220.0 KB** |
| gemma-4-26B asym 2-bit | 977 | 210 MiB | **220.0 KB** |
| qwen3.6-35B asym 2-bit | 1,001 | 86 MiB | **87.7 KB** |
| qwen3.6-35B asym 2-bit | 9,196 | 242 MiB | **27.0 KB** |
| DeepSeek-V4-Flash IQ3_XXS | 369 | 58 MiB | **162.1 KB** |

Two things this settles:

- **The blob scales with tokens, not with the context size.** Byte-identical
  (220,122,770 B) for the same 977-token prefix at `n_ctx=2048` and
  `n_ctx=4096`.
- **KV size is independent of weight quantization.** The 4-bit and 2-bit gemma
  builds cost exactly the same per token; only the architecture matters.

The spread between models is the KV geometry, not the parameter count:
`n_layer × n_head_kv × head_dim × 2 (K and V) × 2 bytes`. gemma-4-26B has 30
layers × 8 KV heads × 256 head dim; qwen3.6-35B has 48 × 4 × 128 — a little
over 8× less per token despite being the larger model.

### Quantizing the cache

Both halves of the cache are now `q8_0` — 8 bits plus a shared scale per 32
values, against f16's 16. Same 1,232-token prefix on gemma-4-26B asym, three
suffixes each:

| k, v | Saved state | Per token | Saving | Prefill | Restore |
|---|---|---|---|---|---|
| f16, f16 | 264.7 MiB | 220.0 KB | — | 34.4 ms/tok | 609 ms |
| q8_0, f16 | 202.7 MiB | 168.5 KB | 23.4% | 38.3 ms/tok | 465 ms |
| **q8_0, q8_0** | **140.6 MiB** | **116.9 KB** | **46.9%** | 39.5 ms/tok | **303 ms** |
| q5_1, q5_1 | 99.3 MiB | 82.5 KB | 62.5% | 50.9 ms/tok | 198 ms |
| q4_0, q4_0 | 74.5 MiB | 61.9 KB | 71.9% | 39.4 ms/tok | 127 ms |

**Restore gets faster, not slower.** The state is written and read as raw rows
of whatever type the cache holds — `state_write_data` records the tensor type
and copies rows, converting nothing — so restore is bandwidth-bound and fewer
bytes is strictly quicker. An earlier note here claimed the opposite from two
single runs on a busy machine; that was noise.

Prefill pays 34.4 → 39.5 ms/token, about 15%. The cache exists to avoid
prefill, so this is the right side of the trade.

**Flash attention is on.** llama.cpp defaults `flash_attn_type` to `auto` and
resolves it by probing the graph; the log says `Flash Attention enabled` on
both models measured here. An earlier note claimed it was off, and used that to
rule out quantizing the value half. It was never checked. Quantizing the value
half is what takes the saving from 23% to 47%.

**Why stop at q8_0.** The table keeps going down and the smoke prompts answer
correctly all the way to `q4_0` — `BANANA`, `42`, three primary colours. That
is not evidence the lower rows are safe; it is evidence the instrument is too
coarse. Cold output from this 2-bit model varies wildly run to run, including
Korean gibberish after a correct answer, so it cannot resolve a small quality
loss. Eight bits is the widely reported near-lossless point; going below it
needs the graded coding suite, not these three questions.

## Speed

gemma-4-26B q4_0, `n_ctx=8192`, greedy decode with a repetition penalty.

| Prefix | Cold turn | Warm turn | Speed-up | Prefill saved | Restore |
|---|---|---|---|---|---|
| 1,050 tok | 42.9–80.6 s | 7.2–43.0 s | 1.9–6.0× | 28.0 s | 861–918 ms |
| 6,685 tok | 270.8–309.5 s | 11.9–27.7 s | **11–18×** | 256.3 s | 313–322 ms |
| 6,685 tok, windowed | 268.0–310.9 s | 10.3–55.5 s | **5.6–28×** | 251.7 s | 105–113 ms |

- **Prefill is 26.7 ms/token at 1k and 38.3 ms/token at 6.7k** — it gets worse
  per token as the prefix grows, so the cache's value grows faster than
  linearly with conversation length.
- **A cold 6.7k-token turn costs five minutes.** That is the number that makes
  this work worth doing: the same turn warm is 12–28 s.
- **Restore is not a flat memcpy, and it is not simply bandwidth-bound.**
  1,436 MiB restored in 322 ms is 4.7 GB/s, while 226 MiB took 900 ms
  (0.26 GB/s) in a different run. Cost tracks the state of the destination
  pages, not the byte count, so the disk tier must be measured with a cold
  page cache before any restore-latency claim is made.

## The sliding-window finding

gemma-4 uses sliding-window attention with `n_swa = 1024` on most layers, so
those layers only ever attend to the last 1024 positions. llama.cpp
nevertheless allocates their KV cache at the full context size by default
(`swa_full = true`), because a windowed cache cannot serve a request that
rewinds inside the prompt.

Fono never set this either way, so it inherited the full-size default. Measured
with the same 6,685-token prefix, changing nothing else:

| | SWA cells | Saved state | Per token | Restore |
|---|---|---|---|---|
| `swa_full = true` | 8,192 | 1,436 MiB | 220.0 KB | 313–322 ms |
| `swa_full = false` | 1,536 | **431 MiB** | **66.0 KB** | **106–108 ms** |

**3.3× less to store and 3× faster to restore, with prefill unchanged**
(38.2 vs 38.3 ms/token). The saving grows with prefix length, because the
windowed layers stay at 1,536 cells while the full-size ones grow.

The saving is invisible below the window: at a 1,050-token prefix both configs
produce a byte-identical 226 MiB blob, because 1,050 cells fit inside the
1,536-cell window with nothing to drop.

Fono leaves `swa_full` at llama.cpp's `true`; the windowed numbers above come
from a temporary build, not from a setting anyone can reach. The measured cost
of the default is a 3.3× larger cache and a 3× slower restore, so switching it
is the single biggest sizing lever available — but see the truncation hazard
immediately below first. Whoever takes it must also fold the choice into the
cache key's runtime identity, or states saved under one layout will be restored
under the other.

### Why windowed mode cannot simply be switched on

The completed-turn checkpoint drops KV cells past the reusable prefix before
serializing (`crates/fono-assistant/src/llama_local.rs:919-925`). With a
full-size cache that is always safe, because every position ever prefilled is
still resident. With a windowed cache it is not: the surviving cells are the
most recent ones, so moving the end of the sequence earlier does not bring back
the positions already evicted, and the window ending at the new boundary can be
missing its oldest cells.

llama.cpp does not detect this. `llama_kv_cache_iswa::state_write` serializes
whichever cells exist, with no check, and a resumed hole does not error — it
silently changes what the model attends to. This is precisely what the
`swa_full = true` default exists to prevent, and no measurement above would have
caught it, because the benchmark never truncates.

Guarded by refusing any truncation while windowed rather than reaching into
llama.cpp for the window geometry. The cost is losing one checkpoint in the one
case where a finished turn's canonical rendering is shorter than what was
generated; the next turn then re-prefills a short suffix.

## Restoring from disk

The restore numbers above all had the blob already in RAM, which is the wrong
assumption for a next-day resume. Measured separately on the NVMe with a
431 MiB file the size of a windowed 6,685-token checkpoint, verified at the
block layer rather than trusted: `/sys/block/nvme0n1/stat` confirms 431 MiB
actually left the device on each cold read and 0 MiB on each warm one.

| | Time | Rate |
|---|---|---|
| Cold, page cache dropped, 1 MiB reads | 56–63 ms | 7.2–8.1 GB/s |
| Cold, 4 KiB reads (the mmap fault pattern) | 91 ms | 5.0 GB/s |
| Warm | 23–27 ms | 17–19 GB/s |

**Reading a checkpoint back from disk costs about 60 ms.** Against the 251,680 ms
cold prefill it replaces, that is four thousandths of one percent. A cold restore
totals roughly 170 ms — the 107 ms restore plus the read — so persistence is
effectively free, and the design is not sensitive to how the blob is read.

Two cautions on these numbers. This is a PCIe 5 drive; on a 500 MB/s SATA SSD
the same read is ~900 ms, which is still three orders of magnitude better than
re-prefilling. And the same arithmetic bounds the interference worry — if
mapping a checkpoint evicts an equal volume of model weights on a
larger-than-RAM model, re-streaming them costs the same ~60 ms spread across
the session.

One measurement trap worth recording: `/tmp` is tmpfs on this host, so the first
attempt at this measurement was reading RAM and reported 9 GB/s "cold". A second
attempt on the NVMe was also wrong, because `cp` sparsifies runs of zeros by
default and the test file had no blocks allocated. Any disk-tier measurement
needs to name its filesystem and confirm the device counters moved.

## What this implies for capacity and for disk wear

Both follow arithmetically from the measured bytes per token, so they need no
further model time.

| Prefix | Full-size | Windowed |
|---|---|---|
| 1,000 tok | 215 MiB | 64 MiB |
| 8,192 tok | 1,760 MiB | 528 MiB |
| 32,768 tok | 7,040 MiB | 2,112 MiB |

**A 4 GiB ceiling holds two 8k checkpoints, or none at 32k.** Windowed it holds
almost eight, and one 32k checkpoint fits. Any ceiling stated in gigabytes has to
be read against these figures rather than against a guess at entry counts.

Churn is the number that is not obviously fine. Only the frontier of a
conversation is kept, so a long stable session writes one whole checkpoint per
turn and deletes the previous one: steady-state occupancy stays low while write
volume does not.

| Session | Full-size | Windowed |
|---|---|---|
| 6,685 tok, 20 turns/hour | **28 GiB/hour** | 8.4 GiB/hour |
| 32,768 tok, 20 turns/hour | **138 GiB/hour** | 41 GiB/hour |

Writing tens of gigabytes an hour to persist a cache is not a reasonable thing
to do to someone's SSD, and it is a cost the current in-RAM cache does not pay
at all. So a disk tier cannot simply write every checkpoint through as it is
created; it needs to hold new checkpoints briefly and only persist those that
outlive the turn that made them, since most are superseded immediately. That is
the opposite of what the cheaper-looking design would do, and it is windowed
mode's second argument: it cuts the write volume by the same 3.3x.

## Correctness

A restored state is not obviously equal to a prefilled one, and a subtly wrong
restore does not error — it quietly produces worse answers. So correctness is
graded, not assumed: the ten coding tasks from
`llm-testing/fono-benchmark/tasks/coding/`, generated once from a cold prefill
and once from a restored state, both graded by the same compile-and-run grader.

**Byte equality is the wrong gate.** A cold arm prefills prefix+suffix in one
batch while a warm arm prefills only the suffix; different reduction orders
move logits by ~1e-6, which flips any near-tie and changes wording without
changing correctness. Both arms are individually deterministic — repeated runs
reproduce byte-for-byte — so the divergence is real, not sampling noise. It
appears at 1,232 tokens and not at 376.

What must hold is the score.

| Run | Prefix | Cold | Warm | Byte-identical | Verdict |
|---|---|---|---|---|---|
| q4_0, `swa_full=1` | 1,050 tok | 7/10 | 7/10 | 3/10 | **pass**, and every task lands the same verdict in both arms |
| q4_0, `swa_full=0` | 6,685 tok | 8/10 | 8/10 | 0/10 | **pass**, and every task lands the same verdict in both arms |
| asym 2-bit, `swa_full=1` | 1,232 tok | 3/10 | 3/10 | 0/10 | passes on aggregate only — see below |

The windowed run is the stronger evidence of the two passes: a 6,685-token
prefix is well past the 1,536-cell window, so the restored state is genuinely
missing the cells that full-size mode would have kept, and it still scores
identically task by task. Its two failures (`rust_expr_eval`, `cpp_dijkstra`)
fail cold as well, so they are the model's ceiling and not the cache's doing.

One cosmetic artefact, recorded because it looks alarming and is not: several
warm replies open with a stray `<|channel>` fragment before the code block. The
grader extracts the code block and is unaffected, and the same tasks pass. It is
the first-token effect of prefilling an 11-token suffix instead of 6,696 tokens
in one batch, not evidence of a bad restore.

The 2-bit run's aggregate match is coincidence: five individual tasks flipped,
three cold→warm and two warm→cold. At 3/10 the model sits on the noise floor,
so that configuration cannot detect a cache defect and is not a usable gate.
The 4-bit run at 7/10 with zero per-task flips is the real evidence.

One fixture trap worth recording: a synthetic prefix built by repeating one
paragraph drives a 2-bit quant into a repetition loop **in the cold arm too**,
which looks exactly like a corrupt restore. Prefixes must be real,
non-repetitive text.

## Defects found and fixed while measuring

- **Sharded models were fingerprinted by their first shard alone.** The cache
  key hashed the size and mtime of the single named file. A GGUF published as
  `…-00001-of-00004.gguf` is handed to llama.cpp as shard 1 only, so for a
  four-shard 104 GB model, 99 GB of weights were invisible to the key —
  swapping them for a different quantization would leave saved states looking
  current. Now every shard is fingerprinted together, in one shared helper used
  by both the assistant and polish backends, and a missing shard fails loudly
  instead of silently shortening the fingerprint.

- **Serializing a truncated windowed KV cache would have written a hole.**
  Found by reading the truncation path while checking whether the sliding-window
  saving could be taken; described in full above. Guarded.

- **A prompt over ~2,048 tokens could not be answered at all.** The whole
  prompt went to llama.cpp in one decode, which refuses a batch larger than
  `n_batch`, so a conversation that grew past the cap failed permanently —
  history only grows. Prefill is now chunked at `n_batch`; positions carry
  across decodes and only the final token asks for logits. The batch
  allocation also drops from context-sized to chunk-sized. This is what
  unblocked every measurement below 32k in this section.

- **A reload returning fewer bytes than it was handed was trusted.** Only a
  zero-byte reload counted as failure. Now every reload site checks the exact
  count. This came out of chasing a suspected large-state defect that proved
  not to exist — see below — but the check is worth keeping on its own.

## The size limit that turned out not to exist

Retracted. This section first reported that llama.cpp reloads a saved state of
a couple of gigabytes incorrectly, and a 1 GiB admission ceiling was added on
that basis. Further measurement showed the reload is exact and the fixture was
at fault. The ceiling has been removed. The reasoning is kept because the
mistake is easy to repeat.

The suspect evidence, gemma-4-26B, same fixture, only prefix length varying —
the warm arm restores and generates greedily, the cold arm reads the whole
prompt:

| Prefix | Saved state | Warm reply |
|---|---|---|
| 6,936 tok | 1.56 GB | byte-identical to cold |
| 8,194 tok | 1.85 GB | substance correct, opening tokens mangled |
| 9,428 tok | 2.12 GB | continues the notes instead of answering |

Three checks then dismantled it.

**The bytes survive.** Saving from the restored context and comparing against
the original gives a diff of 0 bytes at 1.978 GiB, first difference: none. So
nothing is lost or mistranscribed on the way in or out.

**The warm reply is stable across two completely different cache layouts.**
Running the failing prefix with the sliding-window cache allocated at full size
(220 KB/token, 1.978 GiB) and windowed (52.6 KB/token, 0.473 GiB) gives warm
replies that agree for the first 98 characters. A corrupted restore would not
survive a fourfold change in how the state is laid out. In the same pair the
*cold* replies differ from each other — so the cold arm is the unstable one.

**The fixture was ambiguous.** 9,428 tokens of project notes followed by one
line asking a question about them can be read either as a question to answer or
as a document to continue. Three of the four arms continued the document,
including a cold one. Replacing the question with something that cannot be
confused with continuation, at the same 1.978 GiB state:

| Suffix | Cold | Warm |
|---|---|---|
| "Reply with exactly the word BANANA" | `BANANA` | `BANANA` — byte-identical |
| "What is 17 plus 25?" | `42` | `42` (cold emits extra channel markers first) |

So a 2 GiB checkpoint restores correctly. Nothing bounds a single checkpoint
now except the cache's own byte budget.

The lesson worth keeping: a benchmark whose *cold* arm is unstable cannot
measure the warm arm. Both arms must be checked for stability before a
difference between them is attributed to the thing under test — and a model
quantized to two bits, given a long wall of text and a short instruction, is
close enough to a coin flip that it will manufacture a defect on demand.

What survives from this line of work: the strict short-restore check (a reload
returning fewer bytes than it was handed is now refused rather than trusted),
and the per-token cost numbers, which remain the thing that decides whether a
model can hold a long conversation in the cache.

## Peak memory during a save

`/usr/bin/time -v`, gemma-4-26B asym 2-bit (9.60 GB of weights).

| Context | Prefix | Peak RSS |
|---|---|---|
| 8,192 | 8,192 tok (failed) | 11.4 GB |
| 16,384 | 9,428 tok | **17.2 GB** |

The save buffer is allocated at `get_state_size()`, which scales with the
context rather than with the tokens actually present: 3.69 GB at `n_ctx=16384`
against the 2.12 GB actually written. On a 30 GB host that is survivable; on a
16 GB one, raising the context to 16k while running a 9.6 GB model would not
be.

## A model larger than RAM

DeepSeek-V4-Flash IQ3_XXS: 104 GB of weights against 30 GB of RAM, so every
token streams most of its experts from the SSD. Run at low priority to spare
the desktop, which makes the absolute times pessimistic; the ratio is the
point.

| | |
|---|---|
| Prefill | **269 ms/token** (3.7 tok/s) |
| Decode | **~1.4 tok/s** (51 tokens in 37.2 s) |
| Cold turn, 369-token prompt | 114.8 s |
| Warm turn, same prompt | **43.1 s** |
| Restore of the 58 MB state | 213 ms |
| Peak RSS | 28.1 GB |

Three conclusions, and they run against what was assumed when the disk tier was
sketched.

**The cache is worth more here, not less.** Prefill is 7–10× more expensive per
token than on a resident model, so avoiding it saves proportionally more. A
369-token prompt — far too short to be interesting on any other model — already
shows 2.7×.

**Restoring did not visibly cost the weights anything.** The worry was that
mapping a large state would evict weight pages that then have to be re-streamed,
making the cache a net loss. At 58 MB against 104 GB of weights that pressure is
not measurable, and the warm arm stayed 2.7× ahead. The concern only becomes
real for states approaching the size of RAM, and nothing stops one being stored
now — the disk tier will have to bound it.

**"Million-token context" does not imply a cheap KV.** At 162 KB/token this
model is nearly as expensive to checkpoint as gemma-4, and six times worse than
qwen3.6 at the same 9k scale. Whatever the architecture does for context length,
it does not show up as a smaller saved state in this GGUF.

Both replies named the right decision; the warm arm echoed part of the
instruction text before answering.

**It is not usable for coding on this host, cache or no cache.** A modest agent
turn of 10,000 prompt tokens and a 500-token reply costs about 45 minutes of
prefill and 6 minutes of decode. A cache hit removes the first number and cannot
touch the second, so the floor stays in minutes per turn. The cache makes an
unusable configuration less unusable; it does not make it usable.

## Per-token cost is the number that matters

Across four models of broadly similar capability, the cost of remembering a
conversation varies by a factor of eight:

| Model | Per token | 32k-token session |
|---|---|---|
| gemma-4-26B | 220 KB | 7.0 GB |
| gemma-4-26B, windowed | 66 KB | 2.1 GB |
| DeepSeek-V4-Flash | 162 KB | 5.2 GB |
| qwen3.6-35B | 27 KB | 0.9 GB |

qwen3.6 appears to get *cheaper* per token as the prefix grows — 87.7 KB at 1k,
27.0 KB at 9.2k — but that is a fixed ~64 MiB floor being amortised, not a
cheaper model; see the turn-by-turn section below. gemma-4 stays flat at
220 KB with no floor. So model choice, not disk
budget, decides whether a long coding session can be cached at all. A disk
ceiling expressed in gigabytes means something completely different depending on
which model is loaded, which is an argument for reporting the cache's capacity
to users in conversations rather than in bytes.

## A short conversation, turn by turn

Five growing turns on qwen3.6-35B, `n_ctx=8192`. Prefixes are tiny (30 to 184
tokens) because the turns are one sentence each — which is exactly the local
assistant's shape.

| Turn | Prefix | Saved state | Cold prefill | Restore | Warm first token |
|---|---|---|---|---|---|
| 1 | 30 tok | 63.4 MiB | 1.3 s | 98 ms | 264 ms |
| 2 | 67 tok | 64.1 MiB | 1.2 s | 63 ms | 432 ms |
| 3 | 105 tok | 64.9 MiB | 2.8 s | 60 ms | 406 ms |
| 4 | 143 tok | 65.6 MiB | 4.0 s | 60 ms | 471 ms |
| 5 | 184 tok | 66.4 MiB | 5.1 s | 60 ms | 466 ms |

End-to-end turn latency is useless at this scale — it swings between 2.7 s and
37 s depending on how long the model chose to talk, which swamps everything the
cache does. Time to first token is the honest measure, and it holds at
0.26–0.47 s warm against a 1.3–5.1 s cold prefill.

**The important number is the 63 MiB in the first row.** A 30-token checkpoint
costs almost exactly as much as a 184-token one: this model pays a fixed ~64 MiB
floor plus about 20 KB per token. So the per-token figures in the table above
are averages that flatter long prefixes and badly overstate short ones — qwen's
apparent drop from 87.7 KB/token at 1k to 27.0 KB/token at 9.2k is the floor
being amortised, not the model getting cheaper.

That changes what "a hundred small cached items" means. There is no such thing
as a small item here: a hundred one-sentence checkpoints would be 6 GB, not the
tens of megabytes the size of the text suggests. It is another argument for not
writing short local conversation checkpoints to disk at all.

## The API surface

The cache pays off most for a coding agent, so the question was whether the
prompt Fono builds from an API request stays byte-stable turn to turn. Longest-
prefix matching gives nothing at all if the front of the prompt moves.

Read from the request-mapping path rather than measured, because it is settled
by construction:

**Fono introduces no instability of its own.** Multiple system messages are
concatenated in order, history is mapped in order, and nothing time-varying
reaches the prompt text. The wall-clock stamp attached to each replayed turn is
metadata and is never rendered. So an identical `messages[]` array produces an
identical prompt, and prefix stability is entirely a property of the client.
That is the good outcome: the hazards that would kill the hit rate — a
date-stamped system prompt, reordered tool definitions, context compaction that
rewrites the middle of the array — all live on the client side, and a
well-behaved agent avoids them.

**But a coding agent's history did not survive the request until now.** The
request shape carried only `role` and `content`, so `tool`-role messages were
dropped and assistant `tool_calls` were never parsed. Worse, the turn to answer
was taken to be the last *user* message — an agent that has just run a tool
sends the result last, so the call and its result were discarded and an
already-answered question was answered again.

Both are fixed. Tool calls and results now map onto the same history the local
path already renders, and a trailing tool result is what gets answered, labelled
with the same helper the embedded backend replays it with so a prompt continued
mid-exchange stays a prefix of the one rebuilt next turn — which is exactly what
the cached prefix depends on.

**Tool definitions and tool-call replies now work too.** A client's `tools`
array is described in the prompt using the same words and the same order the
embedded backend uses for its own tools, so a model is asked for a syntax it was
actually shown. A reply that contains a call is returned as a `tool_calls` array
with `finish_reason: "tool_calls"` on both surfaces — as a JSON-encoded
`arguments` string on the OpenAI one, as an object on the Ollama one, which is
how each spells it.

Two consequences worth knowing. Offering tools forces the reply to be generated
whole before it is sent, because a call can only be read out of a finished
reply; a client that asked to stream still gets a well-formed stream, just one
that arrives at the end. And a reply is only ever read as a call when tools were
offered — the parser is deliberately tolerant, so on a plain chat turn an answer
that merely discusses JSON would otherwise be swallowed as machinery.

## The GPU on this laptop: 1.5× prefill, 1.9× decode — not 4-6×

An earlier revision of this file claimed 4-6× cheaper prefill from a single pair
of runs. **That claim is retracted.** Repeated on an otherwise idle machine, with
the two arms interleaved so drift shows up as spread, the gain is far smaller and
the earlier figure came from a contaminated CPU arm — those runs shared the
machine with a 104 GB download and a compile.

Two plumbing faults had to be fixed before any of this could be measured at all,
and both are worth recording because either makes a reader conclude the machine
has no usable GPU. `fono-bench` had no passthrough for the *assistant* backend's
`accel-*` features, so `--features accel-vulkan` accelerated whisper only; fixed
by adding `accel-assistant-*` to `crates/fono-bench/Cargo.toml`. And
`streaming_model_params` pins `n_gpu_layers = 0` unconditionally, so the numbers
below come from a temporary override, since removed.

Intel Arc 140V (integrated), Vulkan, 1,232-token prefix, `q8_0` KV, three
process-level repeats per arm, medians:

| gemma-4-26B-asym, 30 layers | CPU | all layers offloaded | ratio |
|---|---|---|---|
| Prefill | 12.91 ms/token | **8.84 ms/token** | 1.46× |
| Decode | 8.94 tok/s | **17.52 tok/s** | 1.96× |
| Restore, 141 MiB | 130 ms | 269 ms | **0.48×** |

| gemma-4-e2b, 34 layers | CPU | all layers offloaded | ratio |
|---|---|---|---|
| Prefill | 3.48 ms/token | **2.31 ms/token** | 1.51× |
| Restore | 70 ms | 29 ms | 2.41× |

Two models a factor of three apart in size, one a mixture-of-experts and one
dense, agree on prefill to within 3%. The loader logs confirm the offload really
happened in both cases — 9,141 MiB of the 26B model on the card, 748 MiB left
mapped on the host.

Run-to-run spread is the reason the earlier number was wrong: prefill varied
9.90-13.43 ms/token on the CPU arm and 7.92-10.00 on the GPU arm. **A single pair
of runs cannot resolve a 1.5× difference.**

### Partial offload: prefill scales, decode falls off a cliff

The five-point curve was first run once per point, and that run was worthless —
each successive load evicted the previous model's pages, so later points paid
disk I/O the earlier ones never did, and one point disagreed with a repeat of the
identical configuration by 2×. Re-run with three repeats per point, interleaved
round-robin, and the model file re-read before every run so all fifteen runs start
from the same warm page cache. gemma-4-26B-asym, 30 repeating layers plus output,
medians of three:

| Layers on card | Prefill ms/token | Decode tok/s |
|---|---|---|
| 0 | 12.81 | 8.53 |
| 8 | 11.84 | 9.36 |
| 15 | 11.66 | **6.07** |
| 23 | 10.33 | 7.83 |
| 31 (all) | **9.93** | **17.57** |

**The two halves of a turn behave completely differently, and only one of them is
linear.**

Prefill declines monotonically and roughly in proportion to the fraction moved —
1.29× end to end, and every intermediate point is a real, ordered share of it.

Decode does not. Full offload is 2.06× and tight (17.32 / 17.80 / 17.57, under 3%
spread). Every partial setting is worthless, and 15 layers is *worse than not
offloading at all*. Against what Amdahl's law predicts from the two endpoints:

| Layers | Measured | Amdahl predicts | Achieved |
|---|---|---|---|
| 8 | 9.36 | 9.83 | 95% |
| 15 | 6.07 | 11.36 | **53%** |
| 23 | 7.83 | 13.80 | **57%** |

The mechanism is batch size, and it explains both rows at once. Each decode call
that spans both devices pays a boundary crossing. Prefill submits 512 tokens per
call, so that crossing is amortised 512 ways and effectively free — hence the
clean linear scaling. Generation submits **one** token per call, so the crossing
is paid in full on every token produced. Split the layers and you buy a fraction
of the compute and pay the crossing regardless; below roughly three-quarters
offloaded the crossing costs more than the compute saved.

This inverts a conclusion recorded earlier in this file. Partial offload was
described as still worth having, as the dial that lets Fono take only the memory
it can spare. It is not: a partial offload chosen to be polite about memory makes
generation slower than leaving the model alone, and generation is what the local
dictation path is made of. **Offload all layers or none.**

Restore behaves differently by model size rather than uniformly. On the 26B model
it is 2× *slower* offloaded, because a restored state is uploaded to the card
rather than memcpy'd within RAM. On the small model it is faster. Either way the
disk tier's arithmetic cannot assume a flat memcpy.

### Offload does not change what the model answers

The two backends reduce in a different order, so replies can differ byte for byte
with nothing wrong — comparing them that way is what produced one of the
retractions below. Score the model instead. Two `release-slim` binaries differing
only in `accel-vulkan`, one isolated profile serving gemma-4-26B-asym as the
assistant over `[server.llm]` at `ctx = 4096`, graded by
`fono-benchmark/capability.py --endpoint` (temperature 0, seed 42; ten tasks, eight
Python, one Rust, one C++). Three repeats per arm, alternating cpu/gpu:

| Arm | Pass, each repeat | Wall | TTFT median |
|---|---|---|---|
| CPU | 7/10, 7/10, 7/10 | 230.7 / 231.8 / 230.7 s | 3.71 s |
| All layers on the card | 7/10, 7/10, 7/10 | 184.2 / 162.3 / 167.1 s | 1.31–1.95 s |

Agreement is per task, not just in the total: the same three tasks fail with the
same failure string in all six runs. Those three are the model and the harness —
one Python syntax error, and two compiled tasks whose replies leak a
`<|channel>thought` preamble that lands a stray backtick in the source — and they
fail identically with no accelerator, so they say nothing about offload.

Wall time here is a by-product; the grader compiles between calls. Read the curve
above for speed.

### Why this is the wrong machine to generalise from

This is an *integrated* GPU: it has no memory of its own, and ggml says so
(`kind=IGpu`, and `fono doctor` prints "shared with system memory"). Offloading
therefore does not move work to a faster memory system — it moves it to a
different compute unit hanging off the same memory controller. A 1.5× gain is
what that should look like.

The consequence for the offload rule is the important part, and it inverts the
emphasis the plan started with:

- **On this laptop, full offload pinned about 10 GB** — 9,141 MiB of weights plus
  467 MiB of KV plus 523 MiB of compute buffers — to buy 1.5×. That memory comes
  straight out of the same pool Firefox uses, and it is *pinned*: weights left on
  the host are mapped page cache the kernel can evict, weights on the card cannot
  be reclaimed until the model unloads.
- **On a discrete card the same 1.5× would cost the desktop nothing**, because
  the memory is a separate pool — and would very likely be larger, since the
  card's own memory is faster than system RAM.

So the rule should not be one budget applied everywhere. Integrated devices want
conservatism, because the win is small and the cost is the user's working memory.
Discrete devices want to offload as much as fits. `ggml_backend_dev_type` already
distinguishes the two, so this needs no heuristic of ours — but it does mean the
numbers above must not be used to size a discrete card, and that no machine other
than an integrated one has been measured yet.

Conservatism cannot express itself as a partial offload, though, because the
curve above shows a partial offload is not a partial win. The only two settings
worth choosing between are all layers and none, so the question on an integrated
device is not *how much* to take but *whether* 10 GB of pinned memory is worth
1.5× prefill and 2× generation on this machine right now.

### Two corrections this run forced

- The `5.9 ms/token` figure previously in this section: single run, busy machine.
- A `5,740 ms → 60 ms` prefill improvement recorded during the offload gates on
  gemma-4-e2b, read as a 95× gain. The CPU arm there ran with a **cold page
  cache**, immediately after a 25 GB model load had evicted everything, so most
  of that 5,740 ms was reading the model file from disk. Measured warm, the same
  comparison is 1.51×.

## A second host: memory the kernel cannot see is still memory

Host: `ai-framework`, AMD Ryzen AI MAX+ 395 with Radeon 8060S (RADV GFX1151),
16 physical / 32 logical cores, **128 GB installed**, Ubuntu 24.04 (glibc 2.39),
Vulkan loader 1.3.275. Also integrated — the same caveat as above applies, and
this still leaves the all-or-nothing rule untested on a discrete card.

**Retraction.** This section previously read "a driver claims is not a budget"
and recorded, as its headline finding, that RADV reports a 111 GB heap on a
machine with 31 GB of RAM — concluding that a device's own figure can only be
believed on a discrete card. That was wrong, and it was wrong because 31 GB was
taken for the size of the machine. The machine has 128 GB in eight 16 GB DIMMs.
Its firmware hands 96 GiB straight to the GPU before Linux boots:

```
amdgpu 0000:c2:00.0: VRAM: 98304M … (98304M used)
amdgpu 0000:c2:00.0:  98304M of VRAM memory ready
amdgpu 0000:c2:00.0:  15934M of GTT memory ready.
```

Linux therefore reports `MemTotal: 31.1 GiB` — everything the firmware left it —
and the kernel's own e820 map shows the hole, 96.8 GiB of "device reserved" from
34.0 GiB to 130.8 GiB. RADV's 111 GB is the exact sum of the two heaps it can
allocate from, 96.0 GiB of private VRAM plus a 15.6 GiB share of what Linux
sees. **The driver was telling the truth to the byte.** `amdgpu` confirms it
independently: `mem_info_vram_total` is `103079215104`.

The consequence was a shipped defect, not a cosmetic one. Bounding this device
by *system* RAM offered a 19 GB budget on a machine with 96 GiB of GPU memory,
so every model between 19 GB and 96 GiB went to the CPU — which is the entire
class of model such a machine exists to run.

The fix needs no vendor interface. Whatever a device reports beyond all the RAM
the kernel knows about is, by definition, memory the kernel cannot allocate, so
it is the device's alone and no reserve protects it. The budget is that plus
what the desktop can spare, capped by the device's own free figure. It
understates the carve-out whenever the driver's total also counts a shared
aperture — here 78 GB against a true 96 GiB — and understating is the direction
that cannot over-commit. It comes out at zero wherever GPU memory is allocated
on demand, which leaves the desktop bound alone in charge, as before.

Same machine, same binary, sizing policy the only difference:

| Model | Sized | Old budget | New budget | Decision |
|---|---|---|---|---|
| DeepSeek-V4-Flash-0731 IQ3_XXS, 4 shards, 104.2 GB | 98.2 GB (97.1 weights + 0.2 cache + 1.0 working) | 11.4–19.4 GB → CPU | 99.7 GB → device | loads, 95.7 GiB resident in the carve-out |
| qwen3.5-2b Q4_K_M, 1.2 GB | 2.2 GB (1.2 + 0.0 + 1.0) | 19.3–19.5 GB → device | device | ~2.9× the CPU arm |

The 97 GiB model loading at all is the proof: 95.7 GiB of it went resident in
VRAM the kernel never counted, with the remainder in the shared aperture, and it
loaded in 62.8 s against 28.6 s for the host path.

### But that model then answers wrongly, for an unrelated and known reason

The offloaded DeepSeek generates degenerate output — `1. 2\n2. 3\n5.\n7,\n11,13`
then a stop, 21 tokens, against a coherent 130-token answer from the host arm on
the same prompt at temperature 0. **This is not the sizing, and not the
carve-out.**

The first attribution written here was wrong and is retracted. It blamed these
load-time warnings:

```
WARN layer 2 is assigned to device Vulkan0 but Lightning Indexer is
     assigned to device CPU (usually due to missing support)
WARN layer 0 … fused DeepSeek V4 HC pre / comb / post … assigned to device CPU
```

Vulkan does lack both ops — `ggml-vulkan.cpp` mentions neither, where CUDA,
Metal (llama.cpp#25893) and SYCL (llama.cpp#26568) implement them. But that
warning *is* llama.cpp disabling the fused path and falling back to unfused
primitives, which is a speed penalty by design, not a correctness one
(`llama-context.cpp`, `resolve_fused_ops`). Missing ops were the wrong culprit.

The real cause is **a llama.cpp older than the fix.** Two attributions were
written here before this one and both were wrong; what follows is what a clean
harness and a version comparison show.

Upstream `llama-cli` build b10405 (`e79e4bf66`), the prebuilt
`ubuntu-vulkan-x64` release, was run on the same machine against the same four
shards with all 62 layers on the device, and it does **not** reproduce. Four
runs, each answering correctly and at length:

| Arm | Result |
|---|---|
| native template, `f16` KV, no penalty | coherent |
| `--chat-template chatml` | coherent |
| `-ctk q8_0 -ctv q8_0`, our prompt | coherent |
| ChatML **and** `q8_0` KV **and** `--repeat-penalty 1.3 --repeat-last-n 128` | coherent |

The last row is every one of Fono's settings at once, and it produced the full
twelve primes followed by the explanation. So neither the quantised cache, nor
the ChatML fallback, nor the repetition penalty causes this on current llama.cpp.

Three further runs then isolate it, and none of them implicate Fono.

*Our decode loop stops for a legitimate reason.* The generator now names the
token that ended a turn, its spelling, and whether the vocabulary treats it as
end-of-generation (`crates/fono-core/src/llama_gen.rs`). On the degenerate
DeepSeek turn it named an end-of-generation token, so the model itself decided
it had finished after nineteen. Nothing was cut short by our stop rule or by the
streamed-marker hold-back. That clears the two suspects an earlier draft named,
and the diagnostic earns its keep: that vocabulary tags **1,277** tokens
`Control` against gemma-4-26B's 16, so "stopped on a control token" alone could
not have told a real end of turn from tool-call or table markup firing mid-answer.

*The same Fono binary is correct on another model.* Swapping the assistant to
`qwen3.5-2b` on the same device arm, same prompt, same settings, returns the
full twelve primes and the explanation. So the decode loop, the ChatML default
and the `q8_0` cache are all fine on that hardware.

*The vendored llama.cpp predates a large DeepSeek-V4 rework.* Hashing the
DeepSeek sources in `llama-cpp-sys-2` 0.1.154 against upstream pins them exactly
to `dee2a846b` (2026-07-27) — both `src/models/deepseek4.cpp` and
`src/llama-kv-cache-dsv4.cpp` match that commit and no later one. Two upstream
commits then touched them:

| Commit | Date | Change |
|---|---|---|
| `596a5795b` (#25784) | 2026-08-02 | DeepSeek-V4 MTP + DSpark — `deepseek4.cpp` +415/−74, `llama-kv-cache-dsv4.cpp` +281/−64 |
| `1269cb1ff` (#26531) | 2026-08-04 | allow reshape of tensors during load |

b10405 contains both and answers correctly; we contain neither and answer
degenerately. That is the whole difference.

There is no fix to apply. `llama-cpp-2` 0.1.154 is the newest published version,
so the vendored llama.cpp cannot be advanced by a version bump, and nothing goes
upstream because upstream already fixed it. The practical statement is narrower
than it looks: **DeepSeek-V4-Flash needs a newer llama.cpp than our binding
vendors, and should not be used with Fono until the binding catches up.** No
policy change, no code change.

For Fono the consequence is narrow. It is not a sizing fault and needs no policy
change. It does mean this architecture cannot serve as the correctness proof for
offload, which is why the graded comparison uses a supported one.

For completeness, since an earlier draft here cited them: llama.cpp#25436
(garbled DeepSeek-V4-Flash on Strix Halo, open since 2026-07-08) is a real open
report, but the clean run above does not reproduce it, so nothing measured here
belongs on that issue. #26685 is a different failure — a CUDA host enabling
fused ops that an RPC-Vulkan worker then skips — and cannot arise single-node.
And `llama-cli --list-devices` independently confirms the carve-out reading
above, reporting 114,238 MiB on a machine where Linux sees 31 GB.

Where the model does fit *and* the architecture is supported, 128 tokens of a
fixed prompt at temperature 0, three repeats per arm, arms alternated twice,
un-niced:

| Arm | Repeats | Median |
|---|---|---|
| CPU (`no accelerator registered`) | 2.24 / 3.73 / 3.82 s and 2.21 / 3.70 / 3.74 s | 3.72 s |
| All layers on the device | 1.41 / 1.26 / 1.29 s and 1.38 / 1.28 / 1.30 s | 1.29 s |

2.9× on wall time, against 1.5× on the Intel laptop — a wider gap, on a wider
memory bus, for a model small enough to be decode-bound. Note the CPU arm's first
repeat is consistently the fastest of its three (2.2 s against 3.7 s); it repeated
across both blocks, and nothing here explains it, so it is recorded rather than
interpreted.

Time to first token is not reported: this server emits its first SSE frame
immediately, so `time_starttransfer` measures the HTTP round trip (~0.3 ms), not
the model.

Two things the four-shard model incidentally proved: `/api/tags` reports
`104,207,848,032` bytes and a digest over all four files, and the load line was
reporting the size of the *first shard only* — 5 MB for a 97 GB model — which is
fixed.

## Where a cached state would land

No state in this session was written to disk: the cache is in memory today and
the disk tier does not exist yet. When it does, the directory it picks needs
checking, not assuming. On the host above, `/` is aufs over tmpfs and `/tmp` is
tmpfs, so the default `$HOME/.cache/fono` **is RAM** — a 4 GiB disk tier there
would consume a seventh of the machine's memory while appearing to spend disk.
`XDG_CACHE_HOME` already redirects the whole cache tree
(`crates/fono-core/src/paths.rs:49`), so no new setting is needed to move it,
but the tier should refuse to grow on a `tmpfs` rather than quietly eat RAM. On
this host the directory was symlinked onto the SSD as a local workaround, which
is enough for measurement and is not a fix.

## Not yet measured

- The API surface end to end: no hit rate, no time to first token over HTTP.
  No longer blocked — tool definitions and tool-call replies both work now.
- 32k contexts. No longer blocked by the batch cap. On gemma-4 a 32k checkpoint
  is 7 GB, so the run needs either a model with a cheaper KV or windowed mode to
  fit alongside the weights.
- A cold page cache. Every restore here ran against pages the same process had
  just written, which is the best possible case and not the one the disk tier
  has to survive.

## Reproducing

```
fono-bench assistant-prefix-cache \
  --model-path <gguf> --prefix-file <real, non-repetitive prose> \
  --suffix '<question>' --ctx-size <n> --batch-size 512 \
  --iterations 1 --out <json>

fono-bench assistant-conversation-cache \
  --model-path <gguf> --ctx-size 8192 --batch-size 512 \
  --turn '<first>' --turn '<second>' ... --out <json>
```

The prefixes used here were built from this repository's own
`docs/decisions/*.md`, truncated to a word count. Prefix length is what varies
between the rows above; everything else is held constant.

One warning about fixtures. The first attempt built prefixes by repeating a
single paragraph, and the cached arm degenerated into a repetition loop. That
looked exactly like a corrupt restore and nearly went into this file as one. It
was the fixture: at 581 tokens the *cold* arm looped too, with no cache
involved. Repetitive text pushes a heavily quantized model into a loop on its
own, so a cache benchmark built on it measures the fixture. Use real prose.
