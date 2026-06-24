# Shared ggml Size-Reclaim Spike

## Objective

Investigate whether Fono can replace the `--allow-multiple-definition`
linker workaround (ADR 0018) with a **single, source-level shared `ggml`
runtime** linked once by both `whisper-rs-sys` (STT) and `llama-cpp-sys-2`
(local LLM). This is a **standalone, read-only-leaning spike** — not part
of the local-TTS critical path. The deliverable is a **decision**, not a
shipped dedup:

1. Re-confirm the ABI / version reconciliation work required between the
   two crates' vendored ggml copies, **on the versions currently pinned**
   (`whisper-rs-sys 0.15.0`, `llama-cpp-sys-2 0.1.150`), since the prior
   spike (2026-05-31) measured against an older `llama-cpp-rs` fork.
2. Decide whether a **forked** or **upstreamed** sys-crate path is viable,
   and which crate carries the change.
3. **Re-measure** the expected binary-size win against ADR 0022's
   standing **~7 MiB** estimate, with a concrete `.text`/section-level
   number rather than an archive-size guess.

Outcome states: **Go** (write an implementation plan + amend ADRs), or
**Defer again** (record fresh findings, keep the link trick, update the
estimate).

## Current State (verified during research)

- The link trick lives in `/.cargo/config.toml:35-47` for
  `linux-gnu` and `windows-gnu`; the dedup invariant is documented in
  `.cargo/config.toml:1-13`.
- ADR 0018 (`docs/decisions/0018-ggml-link-trick.md`) is **Active**; ADR
  0022 (`docs/decisions/0022-binary-size-budget.md:10-13,231-234`) will
  supersede it only once this dedup ("Task 1.2") lands.
- Both sys crates now come from **crates.io** (no `[patch.crates-io]`
  block) per `Cargo.toml:84-99`; the earlier `common`-strip and
  `static-openmp`/`static-stdcxx` patches landed upstream. Lock pins:
  `whisper-rs-sys 0.15.0`, `llama-cpp-sys-2 0.1.150`
  (`Cargo.lock:5107,2458`).
- Prior spike findings (`docs/status.md:2018-2027`): no external-ggml
  CMake knob in `whisper-rs-sys-0.15.0/build.rs` (unconditional ggml
  build at `build.rs:312-316`); the two ggml copies were **different
  revisions** (`ggml.h` diff of 77 lines); only the fork-and-drop-ggml
  path was viable; owner chose to defer.
- Size estimate of **~7 MiB** is currently an inherited figure
  (`docs/binary-size.md:173-187`), never confirmed by direct section
  measurement of the duplicated ggml `.text` in the shipped binary.
- Guardrails: binary-size is the top project priority and is gated in CI;
  the `cpu` `NEEDED` allowlist is exactly four entries; the dedup
  smoke test is `crates/fono/tests/local_backends_coexist.rs`.

## Assumptions

- The spike may add a **throwaway local fork / git checkout** of a sys
  crate for measurement, but no new *shipped* dependency edge is
  introduced without explicit sign-off (per project rules). A
  `[patch.crates-io]` pointing at a fork reuses an already-present crate
  and is net-zero on the dependency graph, so the *mechanism* is allowed;
  the **decision to ship** it is what this spike gates.
- Target of record is `x86_64-unknown-linux-gnu`, `release-slim`, default
  features — the same shape the CI size-budget gate measures.
- "Viable" means: links cleanly, passes `local_backends_coexist`, holds
  the four-entry `NEEDED` allowlist, and the maintenance tail (rebasing
  on upstream sys-crate bumps) is bounded to low single-digit hours.

## Implementation Plan

### Phase A — Re-baseline the facts on current versions

- [x] A1. Pin down the exact ggml provenance each crate vendors today.
  Locate the upstream ggml/whisper.cpp/llama.cpp SHA or release that
  `whisper-rs-sys 0.15.0` and `llama-cpp-sys-2 0.1.150` fetch or vendor
  (inspect each crate's `build.rs`, `CMakeLists`, submodule pin, or
  bundled source tree in the Cargo registry cache). Rationale: the 2026-05-31
  drift was measured against a *fork*; the published `llama-cpp-2 0.1.150`
  may track a different ggml revision, changing the reconciliation scope.

- [x] A2. Re-diff `ggml.h` (and the backend headers — `ggml-backend.h`,
  `ggml-cpu.h`, any `ggml-vulkan.h`) between the two vendored copies.
  Quantify: line drift, added/removed/renamed public symbols, struct
  layout changes, enum value changes. Rationale: ABI compatibility of the
  *surviving* copy is the core risk; the link trick silently keeps one
  set, so any struct/enum drift is latent UB, not a link error.

- [x] A3. Confirm whether an external-ggml build knob now exists in
  either crate's `build.rs` (a `GGML_*` env, a cargo feature, or a
  `links`-key handoff). Re-verify the `whisper-rs-sys` finding (no `links`
  key, unconditional ggml build) on 0.15.0 and check `llama-cpp-sys-2`
  0.1.150 symmetrically. Rationale: a flag-flip path, if it appeared
  upstream since the last spike, collapses the whole effort.

- [x] A4. Survey upstream (`utilityai/llama-cpp-rs`,
  `tazz4843/whisper-rs`) issues/PRs/branches for any existing
  external-ggml / shared-ggml / `system-ggml` work, and whether a
  standalone `ggml-sys` crate either crate could depend on now exists.
  Rationale: an upstreamed path is strictly preferable to a fork for
  maintenance; do not re-invent if upstream is already moving.

### Phase A — Findings (2026-06-24)

- **A1 provenance.** Both crate sources are in the registry cache.
  whisper-rs-sys 0.15.0 vendors **whisper.cpp v1.8.3**
  (`CMakeLists.txt:3`, crate vcs sha `7558e1b…`, repo now
  `codeberg.org/tazz4843/whisper-rs`). llama-cpp-sys-2 0.1.150 vendors
  llama.cpp at crate vcs sha `5459e4d…` (build number injected at CMake
  configure time, not embedded in the package). Their bundled ggml copies
  are **different revisions**: llama's is the newer superset
  (`ggml.h` 107927 B vs whisper's 102112 B).
- **A2 header drift.** `ggml.h` diff: 137 llama-added lines, 16 whisper-only
  (all alignment/deprecation noise — every whisper-referenced symbol
  survives, e.g. `ggml_add1` is now `GGML_DEPRECATED`-wrapped but the symbol
  is intact). `ggml-backend.h` +73/-11, `ggml-cpu.h` +5/-0, `gguf.h` +10/-2.
  Decisive ABI facts:
  - `struct ggml_tensor` is **byte-identical** and all `GGML_MAX_*`
    constants match (DIMS 4, SRC 10, OP_PARAMS 64, NAME 64) → tensor layout
    is safe.
  - `GGML_TYPE_COUNT` 40 → 42 (llama appends `GGML_TYPE_NVFP4=40`,
    `GGML_TYPE_Q1_0=41`) — **additive at the tail**, existing type values
    unchanged → safe.
  - **Hazard:** `enum ggml_op` has a **mid-enum insertion** —
    `GGML_OP_GATED_DELTA_NET` is added before `GGML_OP_UNARY`, shifting
    `GGML_OP_UNARY` and every later op value (incl. `GGML_OP_COUNT`) by +1
    in llama's copy. If whisper.cpp's compiled objects ever compare
    `tensor->op` against an op constant ≥ `UNARY`, linking against llama's
    ggml mis-dispatches → latent UB (Risk #1). Graph *construction* via ggml
    API constructors is safe (op values live inside the surviving ggml.c);
    only direct op-enum reads in whisper.cpp source are at risk. C4 runtime
    smoke test is the gate.
- **A3 build knobs.** whisper-rs-sys 0.15.0: **no external-ggml knob** —
  `build.rs` unconditionally builds + statically links
  `ggml`/`ggml-base`/`ggml-cpu` (`build.rs:312-315`); `links = "whisper"`,
  no `links = "ggml"`. llama-cpp-sys-2 0.1.150: **now has a `system-ggml`
  (and `system-ggml-static`) feature** — sets `LLAMA_USE_SYSTEM_GGML=ON`
  (`build.rs:897-899`) so llama.cpp does `find_package(ggml)` against an
  external ggml, then reads the found lib dirs from CMakeCache
  (`build.rs:979-1005`). This is **new since the 2026-05-31 spike** and
  collapses half the work onto the llama side.
- **A4 upstream.** whisper-rs GitHub repo is an **archived mirror** (read-only
  since 2025-07-30); the live repo is on Codeberg (robots-blocked from here).
  Open GitHub issue **#212 "Add `USE_SYSTEM_GGML`" (Mar 2025) is still
  unimplemented** — the feature is requested but absent from the shipped
  0.15.0. Net: the dedup is **asymmetric** — llama side already supports
  external ggml upstream; the whisper side has no support and must be
  forked/patched (a Codeberg PR is the upstream exit, to be confirmed against
  the live repo when the dedup plan is drafted).

### Phase B — Measure the real size prize

- [x] B1. Build the current canonical artefact
  (`release-slim`, `linux-gnu`, default features) and capture its size +
  `NEEDED` set as the baseline. Rationale: anchors the win against a real
  number, matching the CI gate's measurement shape.

- [x] B2. Quantify the duplicated ggml `.text`/`.rodata` actually present
  in the shipped binary — e.g. by comparing object/section sizes of the
  two ggml builds pre-link, or by a controlled single-engine vs.
  dual-engine link comparison. Rationale: the ~7 MiB figure is an
  archive-size inheritance; `--gc-sections` + LTO may already prune much
  of the duplicate, so the *realised* reclaim could be materially smaller.
  This number decides whether the spike is worth shipping at all.

- [x] B3. Record the measured reclaim against the ADR 0022 ~7 MiB claim
  and note the resulting headroom under the `cpu` cap. Rationale: the
  cap has moved (≤ 32 MiB hard cap, 26-27 MiB enforced row); the offset's
  value depends on current headroom pressure.

### Phase B — Findings (2026-06-24)

Canonical build: `ORT_LIB_LOCATION=<pinned> cargo build -p fono
--profile release-slim --target x86_64-unknown-linux-gnu` (default
features), exactly the CI size-gate shape (`.github/workflows/ci.yml:332`).

- **B1 baseline.** **27,892,632 bytes = 26.60 MiB** (under the 28,311,552 /
  27 MiB `cpu` budget). `NEEDED` is exactly the four-entry allowlist
  (`ld-linux-x86-64.so.2`, `libc.so.6`, `libgcc_s.so.1`, `libm.so.6`).
  `.text` = 21,469,788 B.
- **B2 — realised duplicate ≈ 0.** Relinked a non-stripped twin
  (`--config profile.release-slim.strip=false`) and inspected the symbol
  table:
  - `ggml_init` is defined **exactly once**; **zero** duplicated *global*
    text symbols across the whole binary; **zero** duplicated `ggml_` text
    symbols (561 distinct, each once).
  - The only duplicated *local* symbols are C++ template clones
    (`.isra`/`.constprop`/`.partN`) from onnxruntime / `llama_sampler` /
    gsl / STL — **none are ggml**.
  - Single-copy ggml/quant kernel `.text` ≈ **1.03 MiB** (whole-binary
    defined text ≈ 18.9 MiB / 30,288 symbols).
  Root cause: `.cargo/config.toml:36-41` ships `-Wl,--gc-sections` +
  `-Wl,--as-needed` and `[env] CFLAGS/CXXFLAGS` carry
  `-ffunction-sections -fdata-sections`. `--allow-multiple-definition`
  keeps the first definition of each duplicated ggml global; the loser
  copy's per-function sections become unreferenced and `--gc-sections`
  collects them. The duplicate is **already gone from the shipped binary**.
- **B3 — vs the ADR 0022 ~7 MiB claim.** The ~7 MiB is an *archive-size*
  inheritance (sum of the two sides' `libggml*.a`, ~2.4 MB each pre-link),
  not a section measurement of the linked artefact. The **realised**
  duplicated-ggml reclaim available to a source-level dedup is **≈ 0 MiB**.
  Risk #2 in this plan has materialised: LTO/GC already prune the
  duplicate. The only residual cost of the link trick is **build time**
  (ggml is compiled twice), which is not a binary-size concern, so the
  project's stated top priority is unaffected.

### Phase C — Evaluate the two source-level paths

Phase C is evaluated **on paper only** — with B2 showing a ≈ 0 MiB size
win, prototyping (C4) is not justified. Recorded for the next attempt:

- [x] C1. **Path 1 — fork `whisper-rs-sys` to drop bundled ggml and link
  llama's.** Sketch the patch: gate out whisper-rs-sys's ggml compile,
  resolve its ggml symbols against `llama-cpp-sys-2`'s build, reconcile
  link order. Identify ABI risks surfaced in A2 (whisper.cpp calling a
  ggml API that the llama-tracked ggml renamed/changed). Rationale: this
  was the only path judged viable last time; validate it still is on
  current versions.

  *Eval:* eased by A3 (llama's `system-ggml` exists), but whisper-rs-sys
  needs a fresh fork + the op-enum mid-insertion (A2) reconciled. Note the
  *current* link already produces a **mixed-survivor** ggml (whisper's copy
  wins common globals in link order; llama-only new symbols come from
  llama's copy), so the ABI hazard is already latent today and
  smoke-test-gated — a dedup would not improve correctness. Size win ≈ 0.
- [x] C2. **Path 2 — single shared ggml build both crates compile
  against.** Sketch forking *both* sys crates onto one pinned
  `ggerganov/ggml` checkout (or a shared `ggml-sys`), bumping whisper.cpp
  and llama.cpp to revisions whose ggml is the same family. Rationale:
  cleaner ABI story (one source of truth) but doubles the fork-maintenance
  surface and may force version bumps that ripple into the Rust API.

  *Eval:* doubles the fork-maintenance surface for a ≈ 0 MiB win — worst
  cost/benefit of the three.
- [x] C3. **Path 3 — upstream an `external-ggml` / `system-ggml` feature**
  to whisper-rs-sys (and/or llama-cpp-sys-2). Assess feasibility, likely
  upstream receptiveness (informed by A4), and the interim
  `[patch.crates-io]` bridge while a PR is in flight. Rationale: lowest
  long-term maintenance; aligns with the project's prior success
  upstreaming the `common`-strip and static-runtime patches.

  *Eval:* the *preferred* path **if** the win were real — llama side is
  already upstreamed (A3); only a whisper-rs-sys Codeberg PR would remain.
  But with a ≈ 0 MiB binary-size win it is not worth the upstream effort
  now.
- [~] C4. **Prototype the chosen front-runner far enough to link.**
  **Skipped** — Defer outcome (B2 ≈ 0 MiB); no prototype warranted. Stand
  up a local `[patch.crates-io]` fork implementing the most promising
  path, build the canonical artefact, and confirm: (a) it links, (b)
  `local_backends_coexist` passes, (c) `NEEDED` stays four-entry, (d) the
  Vulkan/`gpu` variant also links (the renamed `ggml_backend_vk_*` symbols
  noted in `docs/status.md:3920-3921` are a known hazard). Rationale: a
  link + smoke-test pass is the minimum bar that turns "viable in theory"
  into a defensible Go.

### Phase D — Decide and document

- [x] D1. Produce the **go/defer recommendation** with the measured
  reclaim (B2), the chosen path (C), ABI-reconciliation scope (A2), and
  the maintenance-tail estimate. Rationale: the spike's actual deliverable.

- [ ] D2. **If Go:** draft a follow-up implementation plan in `plans/`
  (the dedup execution), and stage ADR amendments — ADR 0018 → Superseded,
  ADR 0022 dedup checkbox tied to the real number, `.cargo/config.toml`
  link-trick retirement path. Note: writing these is an *implementation*
  step requiring a build/implementation agent, not Muse.

- [x] D3. **If Defer:** update `docs/status.md`, `docs/binary-size.md:173-187`,
  and the ADR 0022 / ROADMAP estimate with the fresh measured findings so
  the next attempt starts from current facts, and keep the link trick as
  the documented steady state. Rationale: the prior spike's value decayed
  because the numbers were version-stale; refresh them regardless of outcome.

## Decision (2026-06-24): DEFER

**Recommendation: Defer the source-level shared-ggml dedup; keep the
ADR 0018 link trick as the documented steady state.**

- **Why.** The measured duplicated-ggml reclaim in the shipped
  `release-slim` `linux-gnu` `cpu` artefact is **≈ 0 MiB** (B2), not the
  inherited ~7 MiB. The existing `--allow-multiple-definition` +
  `--gc-sections` + `-ffunction-sections/-fdata-sections` combination
  already eliminates the duplicate copy at link time (`ggml_init` once,
  zero duplicated ggml globals). The dedup's headline benefit does not
  exist; only a build-time cost (ggml compiled twice) remains, which is
  out of scope for the size budget.
- **Path, if ever revisited (Go in future).** Path 3 (upstream
  `system-ggml`) is the front-runner because llama-cpp-sys-2 already ships
  it (A3); only a `whisper-rs-sys` Codeberg PR would remain. But the
  trigger should be a *correctness* or *build-time* motivation, not size.
- **ABI note.** The op-enum mid-insertion (`GGML_OP_GATED_DELTA_NET`
  before `GGML_OP_UNARY`, A2) is a latent hazard in the *current* mixed
  link too; it is smoke-test-gated (`local_backends_coexist`) and not made
  better or worse by deferring.
- **Doc reconciliation (D3).** `docs/binary-size.md` §4, ADR 0022's
  Task 1.2 obligation, `docs/status.md`, and `ROADMAP.md` updated to the
  measured ≈ 0 MiB figure; ADR 0018 stays **Active** as steady state
  rather than "interim".

## Verification Criteria

- The exact upstream ggml provenance of both crates at their currently
  pinned versions is documented (SHA/release + header diff metrics).
- A **measured** duplicated-ggml reclaim figure exists for the current
  `release-slim` `linux-gnu` artefact, stated alongside the ADR 0022
  ~7 MiB claim with the delta called out.
- A clear, justified **Go / Defer** recommendation naming one of the three
  paths, with the ABI-reconciliation scope and maintenance tail bounded.
- If Go: a prototype that links, keeps the four-entry `NEEDED` allowlist,
  and passes `crates/fono/tests/local_backends_coexist.rs` on both `cpu`
  and `gpu` variants.
- All ADR / status / roadmap cross-references are internally consistent
  after the spike (no stale "different revisions, ~7 MiB" claims if the
  re-measurement changed them).

## Potential Risks and Mitigations

1. **ABI drift between the two ggml copies is latent UB, not a link error.**
   The link trick already silently keeps one symbol set; a naive shared
   build could compile and link yet crash at runtime when whisper.cpp calls
   a ggml API the surviving copy changed.
   Mitigation: Phase A2 enumerates struct/enum/signature drift up front;
   Phase C4 gates on the `local_backends_coexist` runtime smoke test, not
   just a clean link.

2. **The realised size win is materially below ~7 MiB.** LTO +
   `--gc-sections` may already prune most of the duplicate, making the
   reclaim too small to justify the fork-maintenance tail.
   Mitigation: Phase B2 measures before any fork work; a small number
   flips the recommendation to Defer cheaply.

3. **Forking re-introduces the maintenance burden the project just shed**
   (the upstreamed `common`-strip / static-runtime patches eliminated the
   `[patch.crates-io]` block).
   Mitigation: weight Path 3 (upstream) highest in C3; bound the rebase
   cost in D1; treat a fork as acceptable only with a documented upstream
   exit.

4. **Vulkan (`gpu`) variant breaks** — renamed `ggml_backend_vk_*` symbols
   between the two whisper.cpp/llama.cpp revisions (a hazard already seen,
   `docs/status.md:3920-3921`).
   Mitigation: C4 explicitly links and smoke-tests the `gpu` variant, not
   only `cpu`.

5. **Version churn invalidates findings again.** A sys-crate bump after
   the spike could re-open the drift.
   Mitigation: D3 records SHAs so re-validation is mechanical; the dedup
   plan (if Go) pins both crates and guards the bump in CI per ADR 0022's
   stated approach (`docs/decisions/0022-binary-size-budget.md:283-286`).

## Alternative Approaches

1. **Keep the link trick permanently (no dedup).** Accept the ~7 MiB (or
   less) waste and promote ADR 0018 from "interim" to "steady state",
   retiring the Task 1.2 obligation in ADR 0022. Trade-off: zero
   maintenance, but spends size budget the project treats as its top
   priority; only attractive if B2 shows the realised waste is small.

2. **Dynamic-link one engine as a private `.so`** (the old
   `llama-dynamic-link` idea). Sidesteps ABI reconciliation entirely.
   Trade-off: breaks the single-binary identity (ADR 0005) and adds a
   companion file — contrary to Fono's "one binary" promise; listed for
   completeness, expected to be rejected.

3. **Wait for upstream convergence.** If A4 shows upstream
   `whisper-rs-sys` / `llama-cpp-rs` are independently moving toward a
   shared-ggml or `system-ggml` story, do nothing now and revisit when it
   lands. Trade-off: zero effort, indefinite timeline, no control.
