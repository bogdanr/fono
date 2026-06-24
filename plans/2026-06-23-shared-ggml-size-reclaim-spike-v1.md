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

- [ ] A1. Pin down the exact ggml provenance each crate vendors today.
  Locate the upstream ggml/whisper.cpp/llama.cpp SHA or release that
  `whisper-rs-sys 0.15.0` and `llama-cpp-sys-2 0.1.150` fetch or vendor
  (inspect each crate's `build.rs`, `CMakeLists`, submodule pin, or
  bundled source tree in the Cargo registry cache). Rationale: the 2026-05-31
  drift was measured against a *fork*; the published `llama-cpp-2 0.1.150`
  may track a different ggml revision, changing the reconciliation scope.

- [ ] A2. Re-diff `ggml.h` (and the backend headers — `ggml-backend.h`,
  `ggml-cpu.h`, any `ggml-vulkan.h`) between the two vendored copies.
  Quantify: line drift, added/removed/renamed public symbols, struct
  layout changes, enum value changes. Rationale: ABI compatibility of the
  *surviving* copy is the core risk; the link trick silently keeps one
  set, so any struct/enum drift is latent UB, not a link error.

- [ ] A3. Confirm whether an external-ggml build knob now exists in
  either crate's `build.rs` (a `GGML_*` env, a cargo feature, or a
  `links`-key handoff). Re-verify the `whisper-rs-sys` finding (no `links`
  key, unconditional ggml build) on 0.15.0 and check `llama-cpp-sys-2`
  0.1.150 symmetrically. Rationale: a flag-flip path, if it appeared
  upstream since the last spike, collapses the whole effort.

- [ ] A4. Survey upstream (`utilityai/llama-cpp-rs`,
  `tazz4843/whisper-rs`) issues/PRs/branches for any existing
  external-ggml / shared-ggml / `system-ggml` work, and whether a
  standalone `ggml-sys` crate either crate could depend on now exists.
  Rationale: an upstreamed path is strictly preferable to a fork for
  maintenance; do not re-invent if upstream is already moving.

### Phase B — Measure the real size prize

- [ ] B1. Build the current canonical artefact
  (`release-slim`, `linux-gnu`, default features) and capture its size +
  `NEEDED` set as the baseline. Rationale: anchors the win against a real
  number, matching the CI gate's measurement shape.

- [ ] B2. Quantify the duplicated ggml `.text`/`.rodata` actually present
  in the shipped binary — e.g. by comparing object/section sizes of the
  two ggml builds pre-link, or by a controlled single-engine vs.
  dual-engine link comparison. Rationale: the ~7 MiB figure is an
  archive-size inheritance; `--gc-sections` + LTO may already prune much
  of the duplicate, so the *realised* reclaim could be materially smaller.
  This number decides whether the spike is worth shipping at all.

- [ ] B3. Record the measured reclaim against the ADR 0022 ~7 MiB claim
  and note the resulting headroom under the `cpu` cap. Rationale: the
  cap has moved (≤ 32 MiB hard cap, 26-27 MiB enforced row); the offset's
  value depends on current headroom pressure.

### Phase C — Evaluate the two source-level paths

- [ ] C1. **Path 1 — fork `whisper-rs-sys` to drop bundled ggml and link
  llama's.** Sketch the patch: gate out whisper-rs-sys's ggml compile,
  resolve its ggml symbols against `llama-cpp-sys-2`'s build, reconcile
  link order. Identify ABI risks surfaced in A2 (whisper.cpp calling a
  ggml API that the llama-tracked ggml renamed/changed). Rationale: this
  was the only path judged viable last time; validate it still is on
  current versions.

- [ ] C2. **Path 2 — single shared ggml build both crates compile
  against.** Sketch forking *both* sys crates onto one pinned
  `ggerganov/ggml` checkout (or a shared `ggml-sys`), bumping whisper.cpp
  and llama.cpp to revisions whose ggml is the same family. Rationale:
  cleaner ABI story (one source of truth) but doubles the fork-maintenance
  surface and may force version bumps that ripple into the Rust API.

- [ ] C3. **Path 3 — upstream an `external-ggml` / `system-ggml` feature**
  to whisper-rs-sys (and/or llama-cpp-sys-2). Assess feasibility, likely
  upstream receptiveness (informed by A4), and the interim
  `[patch.crates-io]` bridge while a PR is in flight. Rationale: lowest
  long-term maintenance; aligns with the project's prior success
  upstreaming the `common`-strip and static-runtime patches.

- [ ] C4. **Prototype the chosen front-runner far enough to link.** Stand
  up a local `[patch.crates-io]` fork implementing the most promising
  path, build the canonical artefact, and confirm: (a) it links, (b)
  `local_backends_coexist` passes, (c) `NEEDED` stays four-entry, (d) the
  Vulkan/`gpu` variant also links (the renamed `ggml_backend_vk_*` symbols
  noted in `docs/status.md:3920-3921` are a known hazard). Rationale: a
  link + smoke-test pass is the minimum bar that turns "viable in theory"
  into a defensible Go.

### Phase D — Decide and document

- [ ] D1. Produce the **go/defer recommendation** with the measured
  reclaim (B2), the chosen path (C), ABI-reconciliation scope (A2), and
  the maintenance-tail estimate. Rationale: the spike's actual deliverable.

- [ ] D2. **If Go:** draft a follow-up implementation plan in `plans/`
  (the dedup execution), and stage ADR amendments — ADR 0018 → Superseded,
  ADR 0022 dedup checkbox tied to the real number, `.cargo/config.toml`
  link-trick retirement path. Note: writing these is an *implementation*
  step requiring a build/implementation agent, not Muse.

- [ ] D3. **If Defer:** update `docs/status.md`, `docs/binary-size.md:173-187`,
  and the ADR 0022 / ROADMAP estimate with the fresh measured findings so
  the next attempt starts from current facts, and keep the link trick as
  the documented steady state. Rationale: the prior spike's value decayed
  because the numbers were version-stale; refresh them regardless of outcome.

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
