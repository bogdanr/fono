# Upstream PR: a lost Vulkan device should fail the call, not the process

**Status:** submitted. PR A: <https://github.com/ggml-org/llama.cpp/pull/27183>.
PR B: <https://github.com/ggml-org/llama.cpp/pull/27184> — closed for now;
upstream asks first-time contributors to keep one PR open at a time, so it
reopens after A lands.
**Target:** `ggml-org/llama.cpp` (the Vulkan backend lives in that repo's `ggml/`
tree and is synced out to `ggml-org/ggml`).
**Read against:** the tree vendored by `llama-cpp-sys-2` 0.1.154.

---

## 1. The bug in one paragraph

When a Vulkan device is lost mid-graph — a driver watchdog kills an overrunning
compute job, a GPU reset, a hung queue — `vulkan.hpp` throws
`vk::DeviceLostError`. `ggml_backend_vk_graph_compute` does not catch it, so the
exception unwinds out through `llama_decode`, which is `extern "C"`. Unwinding
across a C boundary is undefined; in practice `std::terminate` runs and the
**whole process aborts**. A caller cannot defend against this: there is nothing
to catch, no status to inspect, and no way to fall back.

## 2. Evidence

Reproduced on Intel Lunar Lake integrated graphics (`Intel(R) Graphics (LNL)`),
Mesa Vulkan, `xe` kernel driver.

**Reproducer — one HTTP request, under four minutes:** prefill ~10,200 tokens of
a 26B model with all layers resident. Any long prefill will do; the trigger is a
compute job that overruns the driver's per-job deadline
(`/sys/class/drm/card0/device/tile0/gt0/engines/ccs/job_timeout_ms`, 5000 ms by
default on this driver).

Kernel side, one per abort:

```
xe 0000:00:02.0: [drm] Tile0: GT0: Timedout job: seqno=308, ... in fono [3416904]
```

Process side:

```
terminate called after throwing an instance of 'vk::DeviceLostError'
  what():  vk::Queue::submit: ErrorDeviceLost
```

**Confirmed cause, by experiment:** raising `job_timeout_ms` to 10000 and
changing nothing else let the identical prompt complete (HTTP 200, 471.6 s, no
abort). The device is lost because the job misses the deadline.

**Ruled out, each by its own arm against the same reproducer:**

| Hypothesis | Arm | Result |
|---|---|---|
| host-tensor op offload | `op_offload` off, all layers resident | still aborts |
| micro-batch size | `n_ubatch` 512 → 128 | still aborts |
| nodes per queue submit | `GGML_VK_MAX_NODES_PER_SUBMIT` 100 → 25 | still aborts |
| cooperative matrices | `GGML_VK_DISABLE_COOPMAT=1`, `..._COOPMAT2=1` | still aborts |
| batch size | `n_batch` 2048 → 256, **3 runs** | 1 completed, 2 aborted |

The batch arm is the informative one: the same setting both survives and dies,
so no caller-side parameter reliably keeps a job under the deadline. **The crash
is not preventable from outside ggml.**

Worth stating plainly in the PR: fixing the exception does not stop the device
being lost. It stops the device loss taking the process with it.

## 3. Why the fix belongs upstream, and why it is small

Three facts from the vendored tree:

1. **The status code already exists.** `GGML_STATUS_FAILED = -1`
   (`ggml/include/ggml.h:360`), and `graph_compute` is declared to return
   `enum ggml_status` (`ggml/src/ggml-backend-impl.h:130`).

2. **The Vulkan backend already catches `vk::SystemError` — seven times** — on
   allocation paths (`ggml-vulkan.cpp:2857`, `:3287`, `:3324`, `:3344`,
   `:3393`, `:3430`, …). It is the compute path that has no handler:
   `ggml_backend_vk_graph_compute` (`:16595`) runs to `:17008` and ends with an
   unconditional `return GGML_STATUS_SUCCESS;`, with no `try` anywhere in the
   body.

3. **Metal already does exactly what is proposed here.** In
   `ggml/src/ggml-metal/ggml-metal-context.m`:

   ```c
   // error state - set when a command buffer fails during synchronize
   // once set, graph_compute will return GGML_STATUS_FAILED until the backend is recreated
   bool has_error;
   ```

   checked on entry at `:438-442` and set on a failed command buffer at `:571`,
   `:586`. So the PR is not proposing a new pattern — it brings Vulkan in line
   with an existing backend.

## 4. The change

### PR A — Vulkan backend (the real fix)

`ggml/src/ggml-vulkan/ggml-vulkan.cpp`:

1. Add a sticky `bool device_lost` to `ggml_backend_vk_context` (or to the
   device, so every backend sharing it fails together — see open question 1).
2. On entry to `ggml_backend_vk_graph_compute`, if set, log once and return
   `GGML_STATUS_FAILED` without touching the device.
3. Wrap the body in `catch (const vk::SystemError & e)`. On
   `vk::DeviceLostError` — or any `SystemError` — log, set the flag, return
   `GGML_STATUS_FAILED`.

Notes for the implementation:

- **Also cover `ggml_vk_synchronize`** and any other submit/wait reachable from
  the compute path; `vk::Queue::submit` is where this one threw, but a fence
  wait can report the same loss.
- **Do not attempt recovery.** A lost device invalidates every object created
  from it; the only correct response is to fail every subsequent call until the
  backend is recreated. That is exactly what Metal's comment says.
- **No interface change**, so no other backend, binding or consumer needs
  touching.

### PR B — llama.cpp public entry points (defensive, separate PR)

`src/llama-context.cpp`: `llama_encode` (`:4075`) and `llama_decode` (`:4086`)
are `extern "C"` and call straight into `ctx->encode` / `ctx->decode` with no
handler. Neighbouring public entry points in the same file already wrap their
bodies in `try` / `catch (const std::exception & err)` — `:883`, `:914`, `:959`,
`:988`, `:1007` and more.

Wrap both in the same pattern, log, and return a negative code. No exception
should cross an `extern "C"` boundary regardless of which backend threw it.

Keep this **separate from PR A**: A is a backend bug fix, B is a hardening
change with a wider blast radius, and reviewers will want to weigh them apart.

## 5. Questions a reviewer may still raise

These were the design choices going in; if review reopens any of them, this is
the reasoning to argue from.

1. **Where the flag lives — context or device.** Several
   `ggml_backend_vk_context`s can share one `vk_device`, and a device lost under
   one is lost under all, so device-level is the more correct scope;
   context-level is the smaller diff.
2. **Whether `ggml_backend_vk_buffer_*` should also fail fast once lost.** Left
   out deliberately to keep the first change small.
3. **Whether the scheduler copes with `GGML_STATUS_FAILED` from a mid-graph
   split.** `ggml_backend_sched_graph_compute` propagates the status; the risk,
   if any, is allocator state left behind that trips a later assert. This is the
   one place the change could break something that currently works, so expect it
   to come up.

## 6. What to include in the PR description

- The two log excerpts from §2 (kernel and process) — they make the failure mode
  unambiguous.
- The `job_timeout_ms` experiment: it identifies the trigger and pre-empts
  "just use a smaller batch".
- The five ruled-out hypotheses table.
- The Metal precedent, quoted. It converts "please add a try/catch" into "please
  make Vulkan consistent with Metal".
- Hardware and driver: Intel Lunar Lake iGPU, `xe`, Mesa. Say it is likely to
  affect any driver with a job watchdog, which is most of them.

## 7. What Fono does in the meantime

`crates/fono-core/src/gpu_offload.rs` writes a marker before handing a prompt to
an accelerated context and clears it when the reply returns. A marker present at
startup can only mean the previous process died mid-generation on that device,
so Fono hides the device for that run (`quarantine_crashed_accelerator`, called
from `crates/fono/src/main.rs` before llama.cpp enumerates) and says why. The
marker is consumed when read, so the next clean start tries the device again.

This is the best available at that layer, and it costs the whole device for a
run. PR A would reduce it to one failed request.

## 8. What happens next

1. **PR A in review.** Answer questions; §5 has the reasoning behind each
   choice.
2. **Reopen PR B once A lands** — upstream asks first-time contributors to keep
   one open at a time.
3. **When A reaches a release Fono pins**, relax the quarantine in §7: a lost
   device becomes a failed request, so Fono can refuse that one request and
   carry on rather than lose the device for the whole run. Do not relax it
   before the pinned `llama-cpp-sys-2` actually carries the fix — the marker is
   harmless when unnecessary and load-bearing when not.

The verification to insist on either way: after A, the reproducer in §2 should
return a decode error and leave the process alive.
