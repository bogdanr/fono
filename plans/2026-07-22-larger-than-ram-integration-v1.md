# Larger-than-RAM MoE — Fono Integration Plan (v1)

**Scope.** Ship the *proven* findings from the research campaign
(`plans/2026-07-21-fono-larger-than-ram-llm-research-v4.md`) into the Fono
codebase, ordered by measured win. This is the implementation counterpart to
that plan's Phase 11 — it turns the ranked thesis into concrete, code-level
tasks against the actual crates. Research/measurement is done; nothing here
re-opens it. Anything unproven (iGPU) or measured-negative (MTP speculation,
CPU prefetch, managed CPU expert cache) is explicitly out of scope.

## Objective

Let a Fono user run a **larger-than-RAM MoE** (the asymmetric 2-bit
routed-expert GGUFs we built and published) as a local **assistant** model
(their primary role — full chat capability, not the short cleanup pass),
streaming cold experts from SSD without OOM, at interactive speed — and make
selecting one as frictionless as possible.

**Role note.** These are assistant-tier models (9.6–11.7 GB, full coding/chat
capability). They are *not* polish/cleanup models — polish stays on the small,
fast defaults (gemma-4-e2b etc.). The registry work below therefore must NOT
bury them under a "polish" label; see Phase C.

## What the research settled (inputs to this plan)

- **Win #1 — no-repack build + mmap on.** The *mechanism gate*: without it the
  stock build repacks Q4_0 into non-evictable anon RAM and disables mmap,
  OOMing under any cap below model size. With it, both target MoEs stream down
  to ~40% resident with no OOM. (`...research-v4.md:28-33`.)
- **Win #2 — asymmetric 2-bit routed-expert imatrix quant.** The headline
  quality/byte win: 9/10 coding at −33% size / −45% cold-bytes on gemma-4-26B
  and qwen3.6-35B (8/10 on ornith-35B). Already built AND **published**:
  - `bogdan-radulescu/gemma-4-26B-A4B-it-asym-GGUF` →
    `gemma-4-26B-A4B-it-asym.gguf` — 9,601,611,488 B (~9.60 GB),
    sha256 `88cca0d55b441627f2c9cb05b5a4752d6bf78b28377ddb4ea0b81675334d8404`
  - `bogdan-radulescu/qwen3.6-35B-A3B-asym-GGUF` →
    `qwen3.6-35b-a3b-asym.gguf` — 11,737,316,320 B (~11.74 GB),
    sha256 `d5d34aba11845c8a6fee4a8007c49989769fa1bc9418a1ad22dbd13faef8a41c`
- **Win #3 — `shared_model` cache-key fix.** Once per-role streaming params
  exist, the current path-only key (`llama_backend.rs:69-72`) would silently
  share the first-loaded variant. Correctness prerequisite, small.
- **Out (measured):** MTP speculation, async expert prefetch, managed CPU
  expert cache — all NEGATIVE in the streaming regime. **Deferred (unproven):**
  iGPU decode + GPU-visible managed cache (research Phase 8).

## Fono facts this plan builds on (verified in-tree)

- **Selection is already role-separate on disk.** The assistant resolves its
  own local model from a **separate** `assistant_models_dir` via its own
  `resolve_local_model_path` (`crates/fono-assistant/src/factory.rs:529-530`),
  keyed off `[assistant.local].model`. Polish has the mirror path
  (`crates/fono-polish/src/factory.rs:118-119`, `[polish.local].model`).
- **But the download *registry* is shared and misnamed.** There is **no**
  assistant registry (`fono-assistant` has no `*_MODELS`/`Registry`); the
  assistant's auto-download piggybacks on `PolishRegistry` — `ensure_models`
  calls `ensure_local_polish` for *both* `config.polish.local.model` AND
  `config.assistant.local.model` (`crates/fono/src/models.rs:88-101`,
  `:322-342`). So `POLISH_MODELS` is *already* the de-facto shared local-LLM
  download registry for both roles; the name is a pre-existing misnomer this
  plan must not deepen.
- **`build_*local` loads *any* file that exists** at the resolved path
  (polish `factory.rs:211-217`; assistant `factory.rs:534-538`); the registry
  is consulted **only for auto-download**. So a manually-placed,
  correctly-named GGUF already loads today for either role.
- **Chat template is picked by model-name substring** — `template_for_model`
  (`fono-polish/src/llama_local.rs:936-942`): name contains `gemma` → Gemma
  template, else ChatML (`qwen3` → thinking suppression). Both published
  filenames satisfy this.
- **The loader uses `LlamaModelParams::default()`** — `shared_model`
  (`crates/fono-core/src/llama_backend.rs:90-92`) — i.e. no explicit mmap/mlock
  control today.
- **llama.cpp build** is the pinned `bogdanr/llama-cpp-rs` sys crate
  (`Cargo.toml:316`); repack/mmap are cmake/load-time levers, not Rust logic.

## Implementation Plan

### Phase A — Win #1: streaming mechanism (mechanism gate; do first)

- [x] Task A.1. **DONE (fork change forwarded, pushed, pinned, env-wired, built).**
  Instead of hardcoding `-DGGML_CPU_REPACK=OFF`, the fork now forwards
  `GGML_`-prefixed env vars into cmake (mirroring the existing `CMAKE_`
  passthrough), so the flag becomes a runtime env toggle and any future ggml
  option is settable without another fork patch. The carry commit `a5abb91` was
  consolidated onto the fono branch **`fono-fgdn-fallback-0.1.150`** (release +
  FGDN + repack) and pushed to `origin`. Done in-repo:
    1. `Cargo.toml` rev bumped `da311b0…` → `a5abb91…` (+ comment) and
       `Cargo.lock` updated (still `llama-cpp-sys-2 v0.1.150`).
    2. `.cargo/config.toml` `[env]` gained `GGML_CPU_REPACK = "OFF"`. Tradeoff
       accepted: disables Q4_0 repack for *all* models incl. the small in-RAM
       gemma-e2b default (minor CPU-matmul perf cost; build-time flag, not
       per-model).
    3. Rebuilt `fono-core --features llama-local` with the new rev; verified
       `GGML_CPU_REPACK:BOOL=OFF` in the freshest sys-crate CMake cache.
  STILL OPEN (moved to the end-to-end gate, Task A.4 below): confirm the shipped
  binary maps a GGUF file-backed (not anon) under an over-RAM cgroup cap
  (streams, does not OOM), mirroring the workbench check.
  A clean, upstreamable version of the same commit is on branch
  **`pr/forward-ggml-env`** (`9492e06`, off `upstream/main`), opened as
  **utilityai/llama-cpp-rs#1079** (OPEN). NOTE: do **not** rebase the fono
  branch onto 0.1.152/main — fono's lockfile pins `llama-cpp-2` **and**
  `llama-cpp-sys-2` at 0.1.150; a sys bump risks bindgen/ABI drift against the
  published 0.1.150 bindings. Moving to 0.1.152 is a separate coordinated bump.
- [x] Task A.2. **DONE.** `shared_model` now has an explicit params path via
  `fono_core::llama_backend::streaming_model_params()` (mmap on, mlock off,
  `n_gpu_layers=0`); the assistant role loads through it
  (`fono-assistant/src/llama_local.rs:308`) while the small dense polish models
  keep `LlamaModelParams::default()`.
- [ ] Task A.3. Size-budget gate: `./tests/check.sh --size-budget` — Win #1 is
  a build-flag + load-param change, expected net-zero on binary size; confirm
  (defer until A.1's fork rev lands).

### Phase B — Win #3: `shared_model` cache-key (land with Phase A)

- [x] Task B.1. **DONE.** `shared_model`'s registry key is now a `ModelKey`
  folding canonical path + `n_gpu_layers` + `use_mmap` + `use_mlock`
  (`llama_backend.rs`), so (a) same file + same params share one resident copy
  and (b) same file + different per-role params load separately. Regression
  tests cover both scenarios plus the streaming-params invariant
  (`llama_backend.rs` `mod tests`, 3 tests passing).

### Phase C — Win #2: ship/select the published asym GGUFs (assistant role)

**Registry naming decision (resolve before coding C.1).** `POLISH_MODELS` /
`PolishRegistry` already serves *both* roles' downloads (see Fono facts), so it
is really the shared local-LLM registry under a legacy name. Do NOT add
assistant-tier MoEs under a "polish" label. Preferred: **generalize the
registry** — rename `PolishRegistry`/`POLISH_MODELS` →
`LocalLlmRegistry`/`LOCAL_LLM_MODELS` (and `ensure_local_polish` →
`ensure_local_llm`), keeping thin `pub use` aliases if churn is a concern — and
tag each entry with the role/tier it targets. Cheaper fallback if the rename is
too broad for this PR: keep the name but add a `tier`/`role` field on
`PolishModelInfo` and mark these `Assistant`. Either way the entries are
assistant-tier, not polish.

- [x] Task C.1. **DONE.** Registry generalized to `LocalLlmRegistry` /
  `LOCAL_LLM_MODELS` / `LocalLlmModelInfo`; both repos registered as
  `gemma-4-26b-a4b-it-asym` (9_602 MB) and `qwen3.6-35b-a3b-asym` (11_737 MB),
  `default_eligible: false`, Apache-2.0, with the concrete `url_path`/`sha256`
  pins (`fono-polish/src/registry.rs`). `ensure_local_polish` renamed to
  `ensure_local_llm` and all callers updated. (A `ModelRole` enum was briefly
  added then **removed** — it had no runtime consumer and an assistant-only tag
  would wrongly forbid using one model for both roles to save RAM; residency is
  a model/machine property, not a role property.)
- [x] Task C.2. **DONE.** Auto-download flows unchanged through the shared
  ensure path (`fono/src/models.rs` `ensure_local_llm`, routed for both roles).
  Registry unit tests added pinning name/size/sha256
  (`registry.rs` `mod tests`, 3 tests passing).
- [ ] Task C.3. **Manual-specify path (see "Manual selection" below).** Document
  that any GGUF dropped at `<assistant_models_dir>/<name>.gguf` with a
  gemma/qwen-containing name loads without registry changes; the registry entry
  is purely the download convenience.
- [ ] Task C.4. Guardrail: these tiers only run acceptably with Phase A. If a
  large MoE is selected on a stock (repack-on) build, surface a clear error/hint
  rather than a silent OOM.

### Phase D — Calibration upgrade (quality, before any re-quant/re-publish)

- [ ] Task D.1. The one open quality lever from the earlier Unsloth comparison:
  a larger, chat-templated imatrix corpus (vs the current text-only ~51k-token
  set). Workbench-side only (`../llm-testing`), offline authoring — **not** in
  the shipped binary. Re-publish the GGUFs (new sha256 → re-pin C.1) only if it
  measurably lifts capability, since re-quant after release is expensive.

## Manual selection — can users specify these easily?

**Yes, two tiers:**

1. **Today, zero code:** download the GGUF, place it in the **assistant** models
   dir as `<assistant_models_dir>/<name>.gguf`, set
   `[assistant.local].model = "<name>"` (and `[assistant].backend = "ollama"` /
   embedded-local, per `build_embedded_local`, `fono-assistant/factory.rs:534`).
   `build_embedded_local` loads any existing file; the registry is only for
   auto-download. Only constraint: the name must contain `gemma` or
   `qwen`/`qwen3` so `template_for_model` (`llama_local.rs:936-942`) picks the
   right chat template. Both published filenames already do. **Caveat:** the
   large MoEs still need Phase A at runtime or they OOM — manual naming doesn't
   bypass the mechanism gate.
2. **After Phase C (recommended):** `[assistant.local].model =
   "gemma-4-26B-A4B-it-asym"` (or the qwen name) auto-downloads from
   `bogdan-radulescu/...` with sha256 verification — no manual file handling.

## Verification Criteria

- On a Phase-A build, a published asym GGUF loads and **streams** (file-backed
  mmap) under an over-RAM cgroup cap without OOM; a stock build OOMs (the
  before/after that proves A.1).
- `fono models install gemma-4-26B-A4B-it-asym` downloads and sha256-verifies
  against the pinned digest; likewise qwen.
- `shared_model` shares one copy for same-file/same-params and loads separately
  for differing params (B.1 regression test green).
- Binary-size budget unchanged by Phases A–C (`./tests/check.sh --size-budget`).
- No new Rust runtime deps; quantization/calibration stays offline in
  `../llm-testing`.

## Risks & Mitigations

1. **Repack-off regresses the small dense default's speed.** Mitigation: scope
   mmap/no-repack to the over-RAM path (A.2); keep `default()` for small models.
2. **User selects a large MoE on a stock build → OOM.** Mitigation: C.4 explicit
   guard/hint.
3. **Re-publishing after D invalidates pinned sha256.** Mitigation: treat D as a
   pre-ship gate; pin C.1 to the *final* artifacts.
4. **Template misdispatch on a manually-named file.** Mitigation: C.3 doc note +
   the load-time `warn_on_template_vocab_mismatch` tripwire already in
   `llama_local.rs:191`.

## Out of Scope (measured/deferred)

- MTP speculative decode (measured negative in streaming this campaign).
- Async expert prefetch, managed CPU expert cache (research Phase 3 negatives).
- iGPU/Vulkan decode + GPU-visible cache (research Phase 8 — unproven; separate
  optional-build effort with its own size-budget check).
