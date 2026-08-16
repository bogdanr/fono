# Task 10 — a real coding agent driving Fono over the HTTP surface

Run record, 2026-08-15, laptop. Companion to
`plans/2026-08-14-disk-kv-cache-evidence-and-design-v2.md` (Task 10, "read the
taxonomy from a real session").

Everything below is measured on this machine unless marked otherwise. Raw logs:
`/tmp/fono-t10/daemon.log`, `/tmp/fono-t10/daemon-gemma.log`,
`/tmp/fono-t10/forge2.log`, `/tmp/fono-t10/mem.log`.

---

## 1. Setup

Fully isolated from the user's own daemon, config and coding session.

| Piece | Isolation |
|---|---|
| Fono daemon | own `XDG_CONFIG_HOME` / `XDG_STATE_HOME` under `/tmp/fono-t10`, LLM server on port **11435** |
| Coding agent | Forge, own `HOME=/tmp/forge-fono-home` — separate credentials, separate database, separate cache |
| Assistant model | `qwen3.6-35b-a3b` (IQ4_XS, 16.9 GB), symlinked into the model dir for the run and removed afterwards |
| Context | 20480 |
| Untouched | the user's `~/.config/fono/config.toml`, the running daemon, the real Forge credentials |

The agent was configured as an ordinary OpenAI-compatible provider pointed at
`http://127.0.0.1:11435/v1`, model chosen from Fono's own `/v1/models` listing.

### Two interop frictions found while connecting

1. **A trailing slash in the base URL breaks model discovery.** The client
   appends `/models`, producing `GET /v1//models`, and Fono answers
   `404 {"error":"not found"}`. Most servers normalise a doubled slash. Cheap to
   fix and it is the first thing a new client hits.
2. **The client caches the model list for seven days on disk.** Changing Fono's
   configured model is invisible until that cache is deleted. Not Fono's bug,
   but worth knowing before debugging a "model not found" that is not Fono's.

---

## 2. First attempt — `gemma-4-e2b`. The agent loop died on turn one.

The model produced the right wrapper with the wrong body:

```
<tool_call>shell("cd /mnt/nvme0n1p5/Work/fono && dd if=/dev/zero of=testfile bs=64M count=2")</tool_call>
```

`parse_call` (`crates/fono-assistant/src/local_tools.rs:203`) requires JSON
inside the wrapper — `{"name": …, "arguments": {…}}`, which is exactly what
`instructions` (`crates/fono-assistant/src/local_tools.rs:71`) shows the model.
A function-call syntax does not parse, so the reply came back as prose with
`finish_reason: "stop"`, the agent saw no tool call, and the session ended after
a single turn.

**Result: two lookups, no reuse, no signal.** A 2B model cannot hold the
protocol well enough to produce agent traffic.

One hypothesis checked and **ruled out**: the voice persona in
`assistant.prompt_main` does *not* leak into HTTP requests.
`split_messages` (`crates/fono-net/src/llm_server/messages.rs:115-120`) folds
the *client's* system messages and uses those. A coding client's own prompt
wins, as it should.

---

## 3. Second attempt — `qwen3.6-35b-a3b`. Full agent loop, and the cache reused.

Task given: measure the machine's NVMe sequential read speed with `dd`, then
report the drive model from sysfs. Four requests, three tool calls, task
completed by the agent unaided.

### 3.1 The cache trace — the Task 10 data

Read from `llm.prompt_cache_lookup` (debug log line at
`crates/fono-assistant/src/llama_local.rs:710-721`).

| # | Prompt tokens | Cause | Matched | Re-decoded prefix |
|---|---|---|---|---|
| 1 | 162 | `runtime_key_change` | 0 | 89 |
| 2 | 10,067 | `divergence` (diverged at index 5) | 0 | 9,877 |
| 3 | 10,566 | **`deepest`** | **9,877** | 319 |
| 4 | 10,686 | **`deepest`** | **10,196** | 402 |

**Zero evictions.** Turns 3 and 4 each reused roughly ten thousand tokens; only
319 and 402 tokens were ever decoded a second time.

The turn-2 `divergence` is not a defect in the cache. The client issues a small
**title-generation** request first, with a different system prompt, and that
request lands in the cache before the agent's real prompt arrives. Consequence:
**every session pays one full cold prefill that a cache could otherwise have
avoided**, because the first slot is occupied by a throwaway.

### 3.2 Timings and memory

| Measurement | Value |
|---|---|
| Model load | **6,095 ms**, 16,909 MB |
| Checkpoint size at ctx=20480 | **212 MB** |
| Cache budget (3 × checkpoint) | **637 MB** |
| Daemon RSS, before load → peak | 12.3 GB → **18.5 GB** |
| System available RAM at the worst point | 24.4 GB of 31.6 GB |
| Turn 1 — title, 162 tokens | 45.2 s |
| **Turn 2 — 9,877-token cold prefill** | **617.2 s** |
| Turn 3 — 9,877 reused, 319 new | 53.4 s |
| Turn 4 — 10,196 reused, 402 new | 65.1 s |

**Turn 2's ten minutes is not prefill speed.** A 16.9 GB mixture-of-experts
model reads scattered expert weights from disk on its first pass; resident size
climbed to 18.5 GB across that turn, and later turns doing comparable work ran
about ten times faster. Quoting 617 s as a prefill figure would be wrong.

No per-token rate is derived here on purpose. The three equations implied by the
turn timings do not admit a consistent prefill/generation pair, which means the
delta counts are not a clean token count. Wall time per turn is what was
measured, so wall time per turn is what is reported.

---

## 4. The agent's own answer was wrong, in two independent ways

It reported **8.0 GB/s sequential read** and drive **Samsung MZVLC1T0HFLU**,
then added commentary.

1. **It benchmarked a RAM disk.** The working directory it was given was under
   `/tmp`, which on this machine is **tmpfs**. It wrote and read a 1 GiB file
   entirely in RAM. It even passed `iflag=direct`, which tmpfs ignores — so the
   flag supplied false reassurance rather than the protection it looks like.
   The directory choice was the operator's mistake; failing to check the
   filesystem was the agent's.
2. **The hardware commentary is fabricated.** It described the drive as "a
   PM9A3 / SM981 derivative using Micron TLC NAND on a Phison E17 controller",
   "PCIe Gen3 x4", ceiling "6.2–6.8 GB/s". An earlier isolated benchmark on the
   same machine established: **Samsung 9100 PRO, PCIe Gen5 x4, 32 GT/s**, cold
   direct reads **7.3–8.2 GB/s**. Controller, NAND vendor, bus generation and
   ceiling are all invented. Only the model string, which it read from sysfs, is
   true.

The single number that looks correct — 8.0 GB/s, close to the drive's real cold
read — is a coincidence: it came from memory, not from the drive.

---

## 5. What this settles, and what it does not

**Settled.**

- The mechanism works against a real coding agent: tool calls round-trip
  through the OpenAI-compatible surface, and the prompt cache matches ~10k
  tokens per turn.
- The taxonomy is readable from a live session without any trace plumbing —
  one log filter is enough.
- The measured checkpoint cost and budget behave as designed: 212 MB per
  conversation, 637 MB budget, and the byte budget never bound.

**Not settled — the gate stays open.**

- Four turns, one conversation, no daemon restart. **Zero evictions**, so the
  evidence so far leans toward Stage B rather than a disk tier, but the sample
  is far too small to rule on.
- `max_entries` (still a hard-coded 10) was never approached, so this run says
  nothing about whether that constant binds before the byte budget does.
- The restart case — the strongest argument for a disk tier — is invisible to
  an in-process taxonomy by construction and was not exercised here.

**New candidate defect — withdrawn on 2026-08-16, see §7.1.** The client's
title-generation request occupies the first cache slot with a prompt that shares
no prefix with the agent's, so the first real turn of every session is a
guaranteed cold prefill.

---

## 7. Second sitting, 2026-08-16: two defects fixed, two more found

Raw logs for this sitting: `/tmp/fono-t10/iq4xs.daemon.log` (first baseline),
`/tmp/fono-t10/asym-truncated.daemon.log`, `/tmp/fono-t10/iq4xs2-posbug.daemon.log`,
`/tmp/fono-t10/iq4xs3.daemon.log`, `/tmp/fono-t10/asym2.daemon.log`, plus the
matching `*.forge.log` and `*.mem.log`.

### 7.1 The title-generation "defect" is withdrawn

The claim in §5 was that the title request poisons the first slot and costs the
first real turn its prefill. It costs nothing. The two prompts share no prefix,
so the title request has no checkpoint to take from the agent, and the agent's
first turn is cold because it is *first* — not because anything displaced it.
Every run in this sitting shows the two as independent misses
(`runtime_key_change` on the 165-token title, `divergence` on the 10k agent
prompt), with the agent's checkpoint intact and reused on the next turn.

Two real observations survive from it. The title request is **concurrent** with
the agent turn and serialises behind it on the model lock, so its wall time
reads as ~10 minutes when it did seconds of work. And it generates to the full
default length for what should be a few words.

### 7.2 Fixed: a doubled slash returned 404

`/v1//models` answered `404 not found`, which is the first thing a client hits
when its configured base URL ends in a slash. The router now collapses repeated
slashes before matching. Verified live during this sitting: the same URL now
returns the model list.

### 7.3 Fixed: the reply length was capped at the spoken-answer default

**This is the defect that invalidated the first comparison attempt.** Every
reply was capped at 384 tokens, and a client's own `max_tokens` could only lower
that, never raise it. A coding agent reasons before it acts and puts the tool
call at the *end* of that reasoning, so the cap cut the call off and the client
was handed prose with nothing to run.

The asymmetric quant died on this immediately: two turns, both stopped at
exactly 384 tokens, mid-sentence, session over. `IQ4_XS` survived only because
it happened to be terser — its turns came in at 108, 74, 28 and 266 tokens.

Now: a caller that names a budget gets it, bounded by a quarter of the context
so the prompt still has room; a client that offers tools and names no budget is
treated as an agent and gets that same ceiling instead of the spoken default; a
plain chat client is unchanged. At ctx=20480 the ceiling is 5120 tokens.

The effect is visible in the numbers: the asymmetric quant's second turn ran to
**1119 tokens** where it had been truncated at 384.

### 7.4 Found and fixed: restoring a checkpoint could fail the request outright

Symptom, seen once the reply cap was lifted and turns got longer: HTTP 500 with
`Decode Error -1`, repeated identically on every retry, killing the session.
Underneath it, llama.cpp reported *"the tokens of sequence 0 in the input batch
have inconsistent sequence positions"* — the restored state's last position was
**11581** while the checkpoint claimed to cover **11549** tokens.

Cause. When a finished turn is checkpointed, Fono first drops the cache cells
past the reusable point so the saved state covers exactly the tokens it records.
The runtime **answers whether it could do that**, and returns false for a partial
rollback it cannot perform — which is the case for this model, whose layers carry
recurrent state alongside attention. Fono checked only that the call did not
error, not what it answered, and filed a state covering the whole turn under a
shorter token count. The next turn then read its suffix into positions the
restored state already occupied, and the runtime rejected the batch.

Two changes. The store side now requires the rollback to have actually happened
before it keeps the checkpoint. The restore side additionally compares the
positions the runtime really restored against what the checkpoint claims, and
starts cold if they disagree — so no future source of the same drift can corrupt
a live session. Both re-runs after the fix show zero occurrences.

This is a correctness bug in the shipped in-memory cache, not in the proposed
disk tier, and it only bites models with recurrent state.

### 7.5 The comparison: IQ4_XS versus the asymmetric quant

Same prompt, same context, same isolated daemon, same client, one after the
other on an idle machine.

| | IQ4_XS | asym (guiq2xxs-dnq2k) |
|---|---|---|
| Weights resident | 16,909 MB | 11,193 MB |
| Load time | 0.9–10.3 s | 8.6 s |
| Checkpoint at ctx=20480 | 212 MB | 212 MB |
| Peak daemon RSS | 19.0 GB | 12.2 GB |
| First agent turn (10k cold prefill) | 610 s | 671 s |
| Reply length, that turn | 108 tokens | 1119 tokens |
| Cached turn (9,889 reused, 242 new) | 47 s | not reached |
| Position errors | 0 | 0 |
| Completed the task | no | no |

**The cache behaves identically on both.** Same checkpoint size, same reuse, same
causes in the same order. Nothing about the storage question distinguishes them,
which is the useful negative result: quantisation choice does not move the disk
tier's economics.

**Neither model completed the benchmark task**, and both failed the same way —
a malformed tool call that Fono's parser correctly refused. `IQ4_XS` emitted
`{"name": "shell",\n command="dd …"}}`, mixing JSON with shell syntax. The
asymmetric quant, after 1119 tokens that included *"Wait I'm going way off track
here"*, emitted `{"command", "arguments": {…}}` — no name, not valid JSON.

**Where they differ is discipline, and it costs real time.** The asymmetric quant
is 5.7 GB smaller and 6.8 GB lighter in peak RSS — meaningful on a 32 GB laptop —
but it rambles: 1119 tokens against 108 for the same turn, and it needed the
raised ceiling merely to reach the end of a thought. On this evidence it is the
better model to *fit* and the worse one to *drive an agent loop with*.

Two cautions on reading this table. Both models are non-deterministic and took
different paths on repeat runs — an earlier `IQ4_XS` run reached eight
generations where the post-fix one reached three — so per-run turn counts are not
comparable. And the ~610–670 s first turns are dominated by paging a large MoE's
expert weights off disk, not by prefill arithmetic; the same work took 47 s once
resident.

### 7.6 What the gate still needs

Unchanged from §5, and still the only thing that can decide Stage C: several
conversations, a daemon restart, and enough turns to fill the entry slots.
`max_entries` remains a hard-coded 10 and was never approached in any run here.

---

## 8. GPU run — same model, same task, Vulkan on the Intel iGPU

Built with `--features llama-local,accel-vulkan`; device reported as
`Intel(R) Graphics (LNL)`. Model `qwen3.6-35b-a3b-IQ4_XS`, `ctx=20480`, same
prompt, same client, same isolation as §7.

### 8.1 Timings, phase by phase

Turn times below are measured from the cache lookup to the end of generation.
The `elapsed_ms` field in the log is not used here because it starts when the
request arrives, so on the GPU run it absorbs the model-load wait.

| Phase | CPU | GPU | Speed-up |
|---|---|---|---|
| Model load | 0.9 s | **191.7 s** | 200× slower |
| Title turn (89 prefix, 384 tokens out) | 44.2 s | 31.6 s | 1.4× |
| Cold agent turn (9,889 prefix) | 609.9 s (108 out) | **96.3 s** (53 out) | ~6.3× |
| Warm agent turn (9,889 matched, ~243 new) | 47.4 s (105 out) | **16.0 s** (111 out) | ~3.1× |

The warm-turn comparison is the fair one: near-identical input, and the GPU
generated *more* tokens in a third of the time, so decode is ~3.3× faster.

**The 191.7 s load is the price, and it is paid once per daemon start.** It is
weight upload, not disk — the CPU run read the same file in 0.9 s from warm page
cache. For a long session it amortises after roughly two turns; for a short one
it dominates.

### 8.2 Cache behaviour — unchanged

| # | Cause | Matched | Re-decoded |
|---|---|---|---|
| 1 | `runtime_key_change` | 0 | 9,889 |
| 2 | `divergence` | 0 | 89 |
| 3 | **`deepest`** | **9,889** | 244 |

Identical to the CPU run apart from turn order (the agent turn arrived first
here because the client raced the title request against a still-loading model).
Same checkpoint size, same 637 MB budget, zero evictions.

**Zero `DeviceLost`, zero decode failures, zero position errors.** The Vulkan
abort recorded earlier in the Stage A sweep did not reproduce: that was `gemma`
past ~8k, and this is a different model at 20k.

### 8.3 Same failure to finish, same cause

The agent ran one shell command, then emitted a malformed tool call —
`{"name": "shell",\n command="dd …"}` — and the session ended. Byte-for-byte the
same failure mode as CPU, which confirms it is model quality and not backend.

### 8.4 Verdict for the long gate run

**Run the gate on the GPU.** A ~3× decode speed-up compounds over a session
long enough to fill the entry slots, which is precisely the session the gate
needs and the reason none of the runs so far reached it. The one-time load cost
is irrelevant at that length, and the cache behaves identically, so nothing
about the measurement is altered by the change of backend.

### 8.5 Unresolved: ~21 GB of RAM unaccounted after the runs

With every benchmark daemon stopped, `free` reports 21 GB used while the sum of
every process's RSS is 413 MB. It is not page cache (1.5 GB), not slab
(0.6 GB), not shmem (0.17 GB), not hugepages, not vmalloc, and `drop_caches`
does not return it. No DRM client remains open other than the desktop's.

It is **not** attributable to the GPU run: the memory was already at 22.5 GB
when that run started, and it climbed during the CPU runs. Left recorded rather
than diagnosed — it is a host-level question, not a Fono one, but anyone
planning a long session on this machine should reboot first.

---

## 9. Gemma 4 26B asymmetric on the GPU — aborted, and two findings

Same harness, same task, `ctx=20480`. Model
`gemma-4-26B-asym-v15-gu.iq2xxs-dn.q2k-pad.gguf` (9.6 GB on disk, 9,156 MB
resident) — the same file the Stage A sweep used.

### 9.1 The cache switched itself off before the first turn

```
WARN prompt cache: 1657 MB budget cannot hold one 2337 MB checkpoint
     at ctx=20480; prompt reuse is off until the context is shorter
     or memory frees up
```

**One Gemma conversation costs 2,337 MB against Qwen's 212 MB — eleven times
more**, for the same context length. The RAM ceiling (a quarter of free memory)
allowed 1,657 MB, so not even one conversation fit and the cache disabled
itself rather than admit and drop on every turn.

The 11× is a difference between the two **models**, not a cost of any setting:
114 KB per token against 10.6 KB. Qwen is a mixture model with very little to
store per token; Gemma is dense with 30 layers.

What `SWA_FULL` costs on top is a separate, smaller number. Gemma reports
`n_swa = 1024`, and llama.cpp then allocates the sliding-window layers at
20,480 cells rather than the 1,536 they can attend to. On a 5:1 window-to-full
layer pattern that predicts ~4.4× at this context, which is consistent with the
3.3× measured at `n_ctx=8192`. A windowed cache would therefore cost roughly
535 MB here — comfortably inside the 1,657 MB ceiling, so the cache would have
stayed on.

This is the first time that guard has fired outside a unit test, and it fired
correctly. It also reframes the storage question: three Gemma conversations
would need ~7 GB of RAM.

### 9.2 The Vulkan abort reproduced, and it now kills the daemon

```
terminate called after throwing an instance of 'vk::DeviceLostError'
  what():  vk::Queue::submit: ErrorDeviceLost
```

Timeline: daemon up 09:26:55, model ready 11.7 s later, title turn served in
10.2 s, agent turn began prefilling 10,201 tokens at 09:27:31, process dead at
09:31:46 — **4 min 15 s into the prefill, 4 min 51 s into the run.**

The Stage A sweep saw this in `fono-bench`, a throwaway process. Here it is the
**daemon**, so the abort takes dictation, the tray, the HTTP server and every
other subsystem down with it. There is no Rust-level catch: the C++ exception
crosses the FFI boundary and `terminate` runs.

Note what did *not* happen: no `Timedout job` appeared in `dmesg` for this run,
and `graph splits = 2` with everything resident, so nothing was being shipped
between host and device. Consistent with the earlier conclusion that submit
size, not `op_offload`, is the trigger.

### 9.3 The numbers up to the crash

| Phase | Qwen IQ4_XS (GPU) | Gemma asym (GPU) |
|---|---|---|
| Weights resident | 16,909 MB | 9,156 MB |
| Model load | 191.7 s | **11.7 s** |
| Checkpoint at ctx=20480 | 212 MB | **2,337 MB** |
| Cache usable | yes | **no — self-disabled** |
| Title turn | 31.6 s | **10.2 s** |
| Cold agent turn (10.2k prefix) | 96.3 s | **aborted at 255 s** |

Gemma loads 16× faster and answers a short prompt 3× faster — it is a dense
9 GB model against a 17 GB MoE. None of that survives contact with a real
coding prompt on this device.

### 9.4 Verdict

**Do not run the long gate on Gemma, and not on Gemma + Vulkan at all.** Two
independent blockers: the process dies at coding-sized prompts, and even if it
lived the cache cannot hold a single checkpoint, so it would measure nothing.

**Run the gate on `qwen3.6-35b-a3b-IQ4_XS` with Vulkan** — the only combination
tested that completes turns, reuses checkpoints, and does so ~3× faster than
the processor.

Two tickets fall out of this, neither blocking the gate:

- **`vk::DeviceLostError` must not abort the process.** A device that is lost
  cannot be recovered on the same context, so the fix is to refuse the request
  and fall back to the processor, not to retry.
- **`SWA_FULL` costs ~4× on checkpoint size at this context.** Worth measuring
  what a windowed cache would cost in correctness before accepting that
  multiplier as permanent.

---

## 10. Chasing the device loss, and what shipped instead

### 10.1 The submit-size hypothesis is dead

The Stage A sweep concluded the trigger was submit size, so the first move was
to shrink the submit: `n_ubatch` cut from 512 to 128, a 4× reduction in the
largest graph handed to the queue in one go.

**It aborted anyway**, at the same point in the same prompt — the first agent
turn, prefilling ~10.2k tokens. Same `vk::DeviceLostError`, same
`terminate called`, same dead daemon.

So submit size is not the trigger either. Both explanations offered so far —
`op_offload` and ubatch — are now retracted by experiment. The remaining
candidates are cumulative: total work submitted across a long prefill, or a
driver-side resource that leaks per submit. Neither is diagnosable from inside
the process, which is the point of 10.2.

The ubatch reduction was reverted; it costs prefill throughput and buys
nothing.

`dmesg` carries **no** `Timedout job` entry for any of these aborts, so the
kernel does not consider the GPU hung. The loss is reported to the client
without the kernel resetting the engine.

### 10.2 Surviving the crash instead of preventing it

A lost Vulkan device throws a C++ exception across the FFI boundary. Rust
cannot catch it; `terminate` runs and the process dies. Nothing inside the
generation call can make that survivable.

What *can* be made survivable is the next start. Fono now writes a marker file
before handing a prompt to an accelerated context and removes it when the reply
comes back. A marker still present at startup means the previous process died
mid-generation on that device — the only way that file survives.

On finding one, Fono runs on the processor for the whole of that run and says
so in plain words:

```
offload: Intel(R) Graphics (LNL) stopped responding during the previous reply
and took Fono down with it; running on the CPU this time. Restart to try the
device again.
```

The marker is consumed when it is read, so the next clean start tries the
device again. A permanently broken device therefore costs one crash per start
rather than a crash loop, and a one-off failure costs a single slow run.

Verified end to end on the laptop iGPU with `gemma-4-26B-asym`:

| Start | Marker on entry | Decision | Outcome |
|---|---|---|---|
| 1 | none | device | aborted mid-prefill, marker left behind |
| 2 | present | **processor** | model ready in 2.8 s, replies served |
| 3 | none (consumed) | device | `19.0 GB available — running on the device` |

Implementation: `crates/fono-core/src/gpu_offload.rs`, wired at the two
generation entry points in `crates/fono-assistant/src/llama_local.rs`.

### 10.3 A four-minute reproducer, and four arms run against it

Driving the agent was never necessary. One HTTP request carrying a 42 KB file
(~10.2k tokens) to the model on the device kills the daemon in under four
minutes, every time. Everything below uses it, so an arm costs one build and
one `curl`.

| Arm | Placement | Outcome |
|---|---|---|
| control | device | **abort at 207 s** |
| `GGML_VK_MAX_NODES_PER_SUBMIT=25` | device | **abort at 239 s** |
| `GGML_VK_DISABLE_COOPMAT=1` + `..._COOPMAT2=1` | device | **abort at 260 s** |
| `GGML_VK_VISIBLE_DEVICES=` (empty) | no device registered | **completed, 708 s, daemon alive** |

**Neither knob helps.** Cutting nodes per submit to a quarter — the real
submit-size control, which `n_ubatch` is not — moves nothing, so the third
size-based explanation dies with the first two. Turning off the
cooperative-matrix paths moves nothing either. Whatever the driver objects to,
it is not the shape of the work.

Only removing the device works, and it works completely: the same prompt
returns a real answer in 708 s with the process still up.

### 10.4 Keeping the layers off the device is not enough

Two arms landed on the processor by accident, because the previous daemon had
not yet released its memory when the next one started. Both **still aborted** —
and the logs say why:

```
Vulkan0 compute buffer size =  1282.53 MiB
graph splits = 695 (with bs=512), 1 (with bs=1)
```

With no layers on the device, llama.cpp still reserves 1.3 GB on it and splits
the work 695 ways. `n_gpu_layers = 0` moves the weights, not the work.

That defeated the guard as first written, which only asked for the layers to
stay home. It now takes the device away instead, before llama.cpp enumerates —
llama.cpp reads `GGML_VK_VISIBLE_DEVICES` at that moment, and an empty value
leaves it with nothing to find. Hence the call at the top of `main`, ahead of
everything.

Verified end to end on the hardened build:

| Start | Marker on entry | Log says | Outcome |
|---|---|---|---|
| 1 | none | `running on the device` | aborted at 248 s, marker left behind |
| 2 | present | `stopped responding … running on the CPU this time` | **zero** Vulkan buffers; the same prompt answered |
| 3 | none (consumed) | `20.8 GB available — running on the device` | device tried again |

### 10.5 The proper fix is upstream, and it is small

Read in the pinned tree (`llama-cpp-sys-2-0.1.154`):

- `ggml_backend_vk_graph_compute` (`ggml/src/ggml-vulkan/ggml-vulkan.cpp:16595`)
  has **no** `try`/`catch` and returns `GGML_STATUS_SUCCESS` unconditionally,
  although the backend interface it implements returns a `ggml_status` for
  exactly this purpose. Allocation paths in the same file do catch
  `vk::SystemError`; the compute path does not.
- `llama_decode` (`src/llama-context.cpp:4086`) is `extern "C"` and does not
  catch either, so a `vk::DeviceLostError` unwinds out of a C function into
  Rust, where unwinding is undefined and the runtime aborts.
- Everything needed to report the failure instead already exists:
  `llama_context::graph_compute` logs and returns whatever status it gets, and
  `llama_context::decode` already maps a failed status to a negative return.

So catching `vk::SystemError` in the Vulkan `graph_compute` and returning
`GGML_STATUS_FAILED` turns a process abort into an ordinary decode error, with
no interface change. A sticky per-device "lost" flag makes later calls fail
fast rather than throw again. That is one narrow patch to ggml, and a second
defensive one wrapping the `llama_decode` / `llama_encode` bodies — llama.cpp
already applies that pattern to its other public entry points.

Both are worth offering upstream. Neither can be done from Fono: a C++
exception cannot be caught across the FFI boundary, which is why the marker
in 10.2 exists.

Two knobs were worth trying first because they cost nothing. Neither helped;
see 10.3.

### 10.6 What is deliberately not done

- **No CPU fallback mid-request.** The process is already gone; there is
  nothing left to fall back with. Recovery is a property of the next start.
- **No user-visible switch.** The quarantine is automatic and self-clearing.
  A setting would need a user to understand a driver failure to set it.
- **The `SWA_FULL` multiplier is untouched.** Still worth measuring, still not
  urgent — it is a cost, not a crash.

---

## 12. The root cause, found — and fixed inside Fono

Section 10 shipped a recovery guard while calling the cause undiagnosed. It is
now diagnosed, and Fono no longer needs to crash first.

### 12.1 A retraction

Earlier sittings recorded "no `Timedout job` in `dmesg`, so the kernel never
considered the GPU hung". That was wrong — the pattern was mistyped. The kernel
logged one for **every** abort:

```
xe 0000:00:02.0: [drm] Tile0: GT0: Timedout job: seqno=308, ... in fono [3416904]
```

The evidence for the crash was in the kernel log the whole time.

### 12.2 The cause is the driver's watchdog

Intel's Xe driver gives a compute job a fixed deadline —
`/sys/class/drm/card0/device/tile0/gt0/engines/ccs/job_timeout_ms`, **5000 ms**
here. A job that overruns is killed and the engine reset; Vulkan then reports
the context lost, and llama.cpp signals that by throwing across its own C entry
point, where Rust cannot catch it. The process dies.

Raising the deadline to 10 s and changing nothing else let the killer prompt
through: **HTTP 200 in 471.6 s, daemon alive, zero aborts.** That is what
identifies the watchdog rather than a fault in the work.

The deadline was restored to 5000 ms afterwards. It is not the fix: it needs
root, it is not portable, and it weakens a protection that exists to keep a
runaway job from freezing the desktop.

### 12.3 Batch size looked like the lever, and is not

Same model, same prompt, stock 5 s deadline:

| `n_batch` | `n_ubatch` | Runs | Outcome |
|---|---|---|---|
| 2048 | 512 | 1 | died |
| 2048 | 128 | 1 | died |
| 512 | 512 | 1 | died |
| 256 | 256 | 3 | **one completed (476.9 s), two died** |

The first run at 256 completed and every larger batch had died, which read as a
cliff between 256 and 512. Two further runs at the same 256, same prompt, same
model killed that reading: the setting does not decide the outcome. Whatever
sits under the deadline varies run to run — thermal state, what else holds the
GPU, how the driver schedules — and a batch size cannot control it.

So the batch cap was **reverted**. Shipping a throughput cost that only lowers
the odds of a crash, without preventing one, buys nothing the recovery guard
does not already give.

The single fact that does hold: raising the kernel deadline to 10 s let the
prompt through in 471.6 s. The watchdog is the mechanism. Nothing reachable from
inside the process reliably keeps a job under it.

### 12.4 One lesson worth keeping from the attempt

The first cut capped the context's batch but left the prefill loop chunking at
the configured value. llama.cpp refused it immediately —

```
GGML_ASSERT(n_tokens_all <= cparams.n_batch) failed
```

— which is the assert doing exactly its job. Any future change to the batch has
to move the context parameter and the prefill chunk together.

### 12.5 The guard is the answer, and the upstream fix is still right

With batch size ruled out alongside micro-batch size, nodes per submit and
cooperative matrices, nothing inside Fono prevents the crash. The
crash-recovery quarantine from section 10 is therefore not a stopgap — it is
the fix available at this layer.

The real repair is upstream, as 10.5 sets out: a driver may lose a device for
reasons no caller controls, and `ggml_backend_vk_graph_compute` should return
`GGML_STATUS_FAILED` rather than let an exception escape into a C caller. The
reproducer is one `curl` and four minutes, and four ruled-out hypotheses make a
far sharper report than before.

---

## 11. Cleanup

Daemon stopped, model symlink removed, temporary config and state directories
retained only as logs. No repository files were changed by this run.
