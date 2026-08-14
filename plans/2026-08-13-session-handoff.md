# Session handoff — 2026-08-13

Scratch note for resuming after a context reset. Delete once the work below lands.

## Git state

- `origin/main` is at `bfe1ca5` — everything since 0.18.1 squashed into one
  (long-prompt chunked prefill, multi-shard fingerprint, API tool support, `q8_0` KV,
  `fono doctor` compute listing, automatic GPU offload and its verification, the Parakeet
  decision, the bench docs). Pushed.
- Branch `main`, **6 commits ahead, unpushed**: `3c47f74` Stage 3 UX (per-role offload
  line in `fono doctor`, the no-knobs config gate, `/api/show` plus real size and digest
  in `/api/tags`), `31cd23d` the firmware carve-out fix, `6e2ceec` and `a15237d` the chat
  format notes and the prompt-cache prefix check, `006bb2c` the stop-token diagnostic,
  `5e34626` the file-name/template disagreement warning.
- Safety branches `backup-presquash-1786574710` and `backup-presquash-task12` hold the
  pre-squash histories and are now redundant — delete them.
- Why the squash: an `--amend` landed on the wrong commit mid-session, so the
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
- **Stage 3 (UX) — done.** `fono doctor` prints a per-role offload line naming the
  device and the arithmetic, recomputed at diagnostic time; a config-serialisation test
  fails the build if any offload knob ever appears.
- **Stage 4 — half done.** Both unified hosts measured; a **discrete card is still
  missing**, so the all-or-nothing rule remains unproven where pinned memory is free.
  Task 16 (re-derive the disk tier) is untouched.

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

1. **Push** — held deliberately. The user pushes only after confirming every open
   problem is closed and the history is squashed. Do not push unprompted.
2. **The disk tier** — the user's original request, still unbuilt. Size it against
   20-220 KB/token, no per-entry ceiling, and add a refusal when the cache directory is
   memory-backed (this machine would otherwise eat RAM while believing it used disk).
3. **A discrete card** is the one measurement left on the offload plan (Task 15).
4. **Render chat prompts with llama.cpp's own template engine, not a name guess.**
   **Done 2026-08-14 for the two families the hand-rolled renderers can express; the fully
   general Jinja path stays out.** `turn_markers_from_template` in
   `crates/fono-core/src/llama_gen.rs` reads the GGUF's own `tokenizer.chat_template` and
   returns the marker pair it names, accepting a candidate only when the template mentions
   both markers *and* the vocabulary registers each as a single control token. Verified
   against three real models: gemma-4-26B → `<|turn>`/`<turn|>` with no help from the file
   name, qwen3.6-35b → ChatML, DeepSeek-V4 → `None` (its template frames roles as
   `<｜User｜>`, which the hand-rolled renderer cannot emit), so the existing warning still
   fires there.

   `resolve_turn_markers` records that answer at load, keyed by the model's file stem, and
   both `turn_markers` and `template_family` read it. That is what makes the model's answer
   reach rendering: the prompt builders are pure functions of the name, called far below the
   loaded model and exercised in tests with no model at all, so recording once at load keeps
   them pure while still following the model. Every name-based `contains("gemma")` dispatch
   in `fono-assistant` and `fono-polish` now goes through `template_family`, so a renamed
   gemma-4 file picks the Gemma renderer *and* the gemma-4 marker spelling. That closes the
   hole the old tripwire could not see: renaming the file used to drop it to ChatML, whose
   markers a Gemma vocabulary happens to register as plain words, so the marker check stayed
   silent while every turn was mis-framed.

   **What is still name-based, and why that is fine.** The ChatML renderer writes
   `<|im_start|>` literally rather than reading it back out of the resolved markers.
   `TurnMarkers::CHATML` is the only ChatML spelling the renderer knows, so the two cannot
   disagree; a family with a different shape resolves to `None` and keeps the warning
   instead. Extending beyond Gemma and ChatML is the Jinja question below, not a marker
   table.

   **A trap worth remembering:** `cargo check --workspace --all-targets` does *not* compile
   `llama_local.rs` for `fono-assistant` or `fono-polish` — `llama-local` is off in their own
   default features, and it only reaches them through the `fono` binary. A missing import
   there passes the workspace check and fails `cargo check -p fono-assistant --features
   llama-local`. Check those two crates with the feature explicitly.

   Today `template_family` dispatches on a substring of the GGUF stem — `gemma` wins Gemma
   markers, everything else falls through to ChatML — but only for a model that named no
   markers of its own. That is why DeepSeek is served
   ChatML, whose `<|im_start|>` tokenizes to six plain-text pieces on its vocabulary instead
   of one control token: out-of-distribution role framing, ~10 wasted context tokens per
   turn that also land in the prompt-cache prefix, and turn termination resting entirely on
   the Control-attribute stop. Measured with upstream b10405, ChatML alone does not break
   that model's answers, so this is a quality tax, not a correctness bug.

   **Scale of the problem, counted rather than guessed** (`llama.cpp/src/llama-chat.cpp`):
   54 named template families, 60 render branches, 55 substring detection branches, and 78
   distinct marker literals. An earlier note here proposed a table of open/close marker
   pairs probed against the vocabulary — **that does not scale and is withdrawn.** Several
   families are not an open/close pair at all: Llama-3 frames roles as
   `<|start_header_id|>role<|end_header_id|>` closed by `<|eot_id|>`, Mistral uses
   `[INST]`/`[/INST]` with no markers, Command-R composes two tokens per role, gpt-oss uses
   channels.

   The cheap fix is that llama.cpp already does all of this and we already link it.
   `LlamaModel::chat_template()` reads the GGUF's `tokenizer.chat_template` and
   `LlamaModel::apply_chat_template()` renders through `llama_chat_apply_template` — both
   safe, both in `llama-cpp-2` 0.1.154, no new dependency and no new bytes. Detection is by
   substring on the template text, so the Jinja inside it never has to be executed. It
   returns an error when it cannot place the template, which is the signal to fall back to
   the hand-rolled path and keep the existing warning. **The reading half is now in use**
   (see above); the rendering half is the part still to do, and note that adopting
   `apply_chat_template` for rendering also retires our own marker table, so the withdrawn
   idea does not come back either way.

   **Keep the gemma-4 special case regardless.** Verified against the GGUF: gemma-4's
   embedded template contains `<|turn>` and none of the strings upstream's detector looks
   for — that file only matches `<start_of_turn>` — so `apply_chat_template` cannot place
   our own default model. Upstream handles gemma-4 only through the real Jinja path in
   `common/chat.cpp`, which is why `llama-cli --jinja` gets it right.

   That Jinja path stays out. It is C++ with no C entry point, so it needs our own shim, and
   minja plus nlohmann is real weight against 23.12 of 25 MiB.

   **The prompt-cache objection is settled — checked, and it clears.**
   `crates/fono-core/tests/chat_template_prefix.rs` renders a three-turn conversation
   through all 52 templates llama.cpp names (no model needed — the API takes the template as
   text) and asserts the prefix property. Rendering the **history** is append-only for
   **52 of 52**. Asking for the trailing assistant header is append-only for 50: `yandex`
   ends the prompt with `[SEP]` and `bailing-think` with `<think>`, and the reply overwrites
   that cue. So the rule is to pin at the end of the history and leave the generation cue in
   the unpinned suffix — which is what our hand-rolled path already does, and both tests are
   permanent so an upstream template edit cannot quietly undo it.

   Worth noting the failure mode is mild anyway: the cache matches on **tokens** at run
   time, so a template that rewrote earlier turns would lose cache hits, not serve a wrong
   prompt. The blocker on adoption was overstated.
5. Deferred: sub-8-bit KV (needs a model whose cold arm is stable — the 2-bit test model
   cannot resolve it), streaming tool calls (saves seconds against prefill's minutes).

## The second host (10.10.0.136)

`ai-framework`, Ryzen AI MAX+ 395 / Radeon 8060S (RADV), **128 GB installed, 96 GiB of
it handed to the GPU by firmware so Linux sees 31 GB**, Ubuntu 24.04 (glibc 2.39). Our
`release-slim` binaries run there unchanged. `rsync` of a binary runs at ~15 MB/s and the
remote's own internet at ~10 MB/s, so fetch models *on* the remote rather than copying
them across.

- Scratch profile `/root/test/prof/{config,data,state,cache}`, with
  `cache/fono/models/polish` symlinked at `/root/test/deepseek-v4-flash-0731-iq3xxs`.
- `/root/test/remote-arm.sh <cpu|gpu>` kills the other arm, starts one detached, waits
  for `Assistant LLM ready`, prints the offload line, then runs `gen-timing.sh` for
  three timed generations.
- Two gotchas: `nohup … &` over a non-interactive ssh dies with the session, so use
  `setsid` and a script (hence `remote-arm.sh`); and `pkill` must match `fono-cpu -v`,
  not the path, or the old daemon survives and silently serves the next arm's numbers.
  That happened once here — the first "gpu" block was really the CPU daemon, caught only
  because the timings matched the CPU arm exactly.
- **Retracted finding.** "RADV reports a 111 GB device heap on 31 GB of RAM, so a
  device's own figure is only safe on a discrete card" was wrong: 31 GB is not the size
  of the machine. 128 GB is installed and firmware carves out 96 GiB, so 111 GB is the
  honest sum of a private heap and a shared aperture. Bounding the device by system RAM
  therefore refused every model between 19 GB and 96 GiB. Fixed by counting whatever the
  device reports beyond `MemTotal` as the device's own; see
  `docs/bench/prompt-cache-2026-08-11.md`.
- **A model can offload and still answer wrongly.** The 97 GiB DeepSeek loads on the
  device and generates degenerate text. **Two attributions were wrong; do not post
  upstream.** First blamed on Vulkan lacking DeepSeek V4's Lightning Indexer and fused HC
  attention — those `assigned to device CPU (usually due to missing support)` warnings are
  llama.cpp disabling the fused path for unfused primitives, costing speed, not
  correctness. Then blamed on llama.cpp#25436. Upstream `llama-cli` b10405
  (`ubuntu-vulkan-x64` prebuilt) on the same machine and shards, all layers on the device,
  answers correctly under every one of Fono's settings — ChatML template, `q8_0` KV, and
  `--repeat-penalty 1.3 --repeat-last-n 128`, together and separately.
  **Resolved 2026-08-13: the vendored llama.cpp is simply too old.** Three runs isolate
  it. Our new diagnostic names the stopping token, its spelling and whether the vocabulary
  treats it as end-of-generation (`crates/fono-core/src/llama_gen.rs`); on the degenerate
  turn it named an end-of-generation token, so the model itself ended after nineteen and
  neither the Control-token stop nor the streamed-marker hold-back cut anything short. That
  diagnostic is worth keeping: DeepSeek-V4-Flash tags 1,277 tokens `Control` where
  gemma-4-26B tags 16, so the bare "control_token" stop reason cannot distinguish a real end
  of turn from table or tool-call markup firing mid-answer. The same binary
  on the same device arm answers `qwen3.5-2b` correctly. And hashing the DeepSeek sources
  in `llama-cpp-sys-2` 0.1.154 pins both `src/models/deepseek4.cpp` and
  `src/llama-kv-cache-dsv4.cpp` to upstream `dee2a846b` (2026-07-27), before #25784
  (2026-08-02, +415/−74 and +281/−64 on those two files) and #26531 (2026-08-04). b10405
  has both. **DeepSeek-V4-Flash is unusable with Fono until the binding catches up**; pick
  another architecture for correctness work.
- **A bump is in flight upstream: llama-cpp-rs#1097**, `../llama-cpp-rs` branch
  `pr/bump-llama-cpp-b10405`, moving the submodule b10200 → b10405. It is not a free bump:
  two sampler entry points changed in that range, so the safe wrappers move with them.
  `llama_sampler_init_penalties` gained a leading `n_vocab` (llama.cpp#26520), which makes
  `LlamaSampler::penalties` a breaking change and matches how `mirostat` and `logit_bias`
  already take it; `llama_sampler_init_dry` dropped `n_ctx_train` (llama.cpp#26524), which
  the wrapper simply stops passing, so its own signature is unchanged. That second change
  also silently redefines `penalty_last_n = -1`: it used to mean the context size and is
  now clamped to 0, which *disables* the penalty — a behaviour flip with no compile error,
  documented on the wrapper. Fono passes `PENALTY_LAST_N = 128`, so the flip does not
  reach us, but the `n_vocab` change will: `crates/fono-core/src/llama_gen.rs:93,135` are
  the two call sites that must gain the argument when the binding is bumped. Once it lands
  and a release follows, re-test DeepSeek-V4-Flash before lifting the warning above.
- **Upstream contribution check, done 2026-08-13.** Nothing to send *to llama.cpp* — the
  one thing worth sending went to the binding instead, as #1097 above. Vulkan DSV4 HC fused
  ops are already taken: llama.cpp#26548 → #26578 by kh0pper on the same gfx1151 hardware,
  blocked behind their split-out transpose #26585 (open). Vulkan `LIGHTNING_INDEXER` is
  genuinely absent where Metal (#25893) and SYCL (#26568) have it, but it is a shader job
  and pure speed. #25436 is real and open, but we cannot reproduce it with a clean harness,
  so we have nothing to add to it.
- **Reproduce with `llama-cli` before blaming a backend.** The prebuilt release tarball
  (`gh release view -R ggml-org/llama.cpp`) drops a working Vulkan `llama-cli` on a remote
  box in under a minute — no build. Doing that first would have saved two wrong write-ups
  and a nearly-posted issue comment. Use `-st` for a single turn; without it the binary
  loops on stdin EOF and wrote a 1.6 GB log of spinner frames.
- Detached work over ssh: write the script in one call, run it in another. A backgrounded
  `&` inside the ssh command string holds the channel open and the call times out.
- 10.10.0.136 has only 31 GB the kernel can use. A 95.7 GiB model resident plus a 34 GB
  download through page cache filled swap and wedged the box for ~10 minutes, killing both
  the daemon and the download. Do one at a time there.

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

## Verifying the carve-out on the second host

`/root/test/` holds `fono-gpu`, built with the previous policy, which makes the sizing
policy the only variable against a current build. `oldpolicy-run.sh` starts it detached;
`timing-detached.sh <label>` runs `gen-timing.sh` without the caller having to outlast it.
Read the resident figure straight from the driver:
`/sys/class/drm/card1/device/mem_info_vram_used`. The probe binaries built for the
carve-out and stop-token work were removed once they had answered; rebuild and `rsync` one
when needed, and free the page cache first (`echo 3 > /proc/sys/vm/drop_caches`) or a
97 GiB model will not fit even though it did an hour earlier.

A second correctness arm is not currently staged. Qwen3-32B-Q8_0 was fetched for it and
then deleted as too old a model to be worth the disk; use qwen3.6-27b if one is wanted,
fetched on the remote and run with nothing else loaded, or the box swaps itself to a
standstill. The carve-out itself needs no such proof: upstream `llama-cli --list-devices`
reports 114,238 MiB on a box where Linux sees 31 GB, which is independent confirmation.

## Comparing against upstream llama.cpp on the second host

`/root/test/llamacpp/llama-b10405/` is the prebuilt `ubuntu-vulkan-x64` release, which is
the fastest way to tell a Fono defect from a llama.cpp one. `dsv4-repro.sh <tag> <ngl>
[extra args…]` runs one greedy fixed-seed generation and writes `cli-<tag>.log`;
`dsv4-launch.sh` detaches it, and `PROMPT` overrides the prompt. Run `LD_LIBRARY_PATH=.`
from that directory. Reach for this **before** attributing anything to a backend.

## Verifying offload does not change answers

Build two `release-slim` binaries differing only in `accel-vulkan`, point an isolated XDG
profile at a model that fits the card, serve it as the assistant over `[server.llm]`, and
grade both with `fono-benchmark/capability.py --endpoint`. The scratch profile and the
six result files live under `/mnt/150g/task12/` (`run-arm.sh <cpu|gpu> <rep>` reruns one
arm end to end). Compare *scores*, not bytes.
