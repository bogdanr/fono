# Fono Code Audit — Suggestions-Only Programme

## Objective

Systematically review ~156,500 lines of Rust across 19 workspace crates to surface
**bugs, logic errors, and improvement opportunities**. The deliverable is a
**reviewed, severity-ranked findings ledger** — not code changes. No source file is
modified during Stages 1 and 2. Fixes happen only in Stage 3, only for findings the
user has explicitly approved.

### Three-stage contract

| Stage | Output | Writes code? |
|---|---|---|
| **Stage 1 — Sweeps** | Mechanical, tool-derived signal: coverage map, dependency/lint gaps, unsafe inventory, dead code, feature-matrix validity | No |
| **Stage 2 — Reading** | Per-unit human-style review of logic, concurrency, error paths, and tests, unit by unit | No |
| **Stage 3 — Fixes** | Patches, one approved finding (or one tight cluster) per commit | Yes, after approval |

### Scope boundaries

- **In scope:** all `crates/*/src`, `crates/*/tests`, `crates/*/examples`, `build.rs`,
  `tests/check.sh`, `.github/workflows/`, `deny.toml`, `Cargo.toml` lint/feature config.
- **Out of scope:** `docs/bench/calibration/runs/` (3,044 inert result JSONs — exclude
  from every file-count, churn and grep metric), `plans/`, `calibration/`,
  `clean-sweep-runs-local/`, vendored/generated tables (e.g.
  `crates/fono-tts/src/supertonic/nfkd_table.rs`).
- **Deliberately not re-litigated:** the binary-size budget design, the 14 documented
  `deny.toml` advisory ignores, and the ADR decisions. The audit *verifies the stated
  exit conditions still hold*; it does not reopen the decisions.

### Assumptions made

- The audit runs against a **frozen commit SHA** recorded at kickoff, so line citations
  in findings stay valid. If `main` moves, findings are re-anchored, not re-derived.
- "Improvement opportunities" includes maintainability and correctness-risk reduction,
  but **excludes** stylistic churn and excludes anything that grows the shipped binary
  without a stated size estimate.
- Cross-platform code (macOS/Windows backends) is audited by reading only; no attempt
  to obtain those hosts.

---

## Review Units

Fourteen units, ordered by **risk × concentration**, not by dependency tier. The two
6,500-line files in `fono` are split so no unit exceeds roughly 4,000 lines of review.

| # | Unit | Primary surface | Approx. lines | Why this rank |
|---|---|---|---:|---|
| A | Session orchestration | `crates/fono/src/session.rs` | 6,800 | Largest file; mixes `std` and `tokio` sync primitives, 43 spawn sites |
| B | Daemon event loop | `crates/fono/src/daemon.rs` | 6,600 | Fans FSM events to tray/IPC/overlay; 33 spawn sites |
| C | Assistant runtime | `crates/fono/src/assistant.rs`, `crates/fono/src/wake.rs` | 4,000 | Densest shared-state file in the workspace (66 constructs) |
| D | State machines | `crates/fono-hotkey/`, `crates/fono-overlay/src/lib.rs` state enum | 2,600 | The only formal FSM; guards three mutually-exclusive pipelines |
| E | Audio real-time path | `crates/fono-audio/` | 7,100 | Capture/playback, VAD, wake-word, speaker DSP; 19 shared-state constructs in playback alone |
| F | Untrusted input decoders | `crates/fono-net-codec/`, `crates/fono-mcp-server/src/protocol.rs`, `crates/fono-ipc/` | 2,000 | Only surfaces that parse bytes from off-process/off-host |
| G | Network servers | `crates/fono-net/` | 7,300 | Three concurrent servers plus auth and mDNS |
| H | FFI and `unsafe` | `vk_loader_shim.rs`, `brain_tap.rs`, `hwcheck.rs`, `vulkan_probe.rs`, overlay backends, `fono-inject/src/{focus,permissions}.rs` | ~2,500 of unsafe-adjacent | 53 of ~160 unsafe sites in two files |
| I | Local inference backends | `fono-assistant/src/llama_local.rs`, `fono-polish/src/llama_local.rs`, `fono-stt/src/whisper_local.rs`, `fono-core/src/llama_gen.rs` | 7,200 | Blocking threads, model lifetime, cancellation |
| J | Config, paths, persistence | `crates/fono-core/src/config.rs` + storage/secrets modules | 6,000 | Most widely-depended-upon file; migration and secret handling |
| K | Cloud provider backends | `fono-stt`, `fono-tts`, `fono-polish`, `fono-assistant` remote modules + `fono-http` | 12,000 | Shape-identical by design — audit for *divergence* between siblings |
| L | Injection and platform integration | `crates/fono-inject/`, `crates/fono-tray/` | 5,900 | Five injection backends; clipboard restore correctness |
| M | Rendering and overlay | `crates/fono-overlay/src/renderer.rs`, `r3d.rs`, `cortex.rs`, four backends | 8,800 | Carries the most blanket `#![allow(...)]` escapes |
| N | Update, download, bench, CLI surface | `fono-update`, `fono-download`, `fono-bench`, `crates/fono/src/{cli,wizard,installer,doctor}` | 12,000 | Atomic-replace correctness; wizard is the first-run blast radius |

---

## Implementation Plan

### Phase 0 — Set up the ledger and rubric (do this before reading any code)

- [x] Task 0.1. Record the audit anchor: capture the exact commit SHA the audit runs
      against and note it at the top of the ledger, so every `file:line` citation stays
      resolvable for the life of the programme.
- [x] Task 0.2. Create the findings ledger at `docs/audit/findings.md` (single file,
      append-only during Stages 1–2). One row per finding with a **stable ID**
      (`F-<unit><nnn>`, e.g. `F-A007`), so Stage 3 approvals and commits can reference
      IDs rather than prose. Rationale: findings must outlive the session that produced
      them; a chat transcript is not a deliverable.
- [x] Task 0.3. Define the finding record format, and require every field:
      **ID · Unit · Severity · Confidence · Location (`file:line`) · Observation ·
      Why it is wrong / suboptimal · Reproduction or "theory-only" · Suggested
      direction · Estimated blast radius · Binary-size impact (if it implies a new
      dependency)**. Rationale: the "confidence" and "reproduction" fields are what stop
      a static-reading audit from flooding Stage 3 with false positives.
- [x] Task 0.4. Adopt the severity rubric and write it into the ledger header:
      **S1 Correctness-critical** (data loss, hang, crash, wrong output reaching the
      user, secret leak) · **S2 Behavioural defect** (wrong under a reachable but
      non-default condition) · **S3 Latent risk** (works today, fragile under change —
      races, unchecked invariants, TOCTOU) · **S4 False-confidence test** (a test that
      passes while the mechanism it names is disarmed) · **S5 Improvement**
      (clarity, duplication, dead code, ergonomics). Rationale: without this, the
      programme produces an undifferentiated list nobody can triage.
- [x] Task 0.5. Adopt the recurring audit **lenses** — the fixed question set applied to
      every unit, so coverage is uniform rather than dependent on what catches the eye:
      (1) *Cancellation & shutdown* — can every spawned task be stopped, and what happens
      to in-flight work? (2) *Error paths* — is every `?`/`unwrap`/`expect` on a path a
      user can reach, and what do they see? (3) *Concurrency* — lock ordering, held-across-
      await, `std::sync` inside async, atomics used as channels. (4) *Invariants* — what
      does this module assume that nothing enforces? (5) *Boundary values* — empty input,
      zero-length audio, unicode, very long text, clock going backwards. (6) *Resource
      lifetime* — files, sockets, model handles, temp files, on both success and failure.
      (7) *False-confidence tests* — does the test exercise the mechanism or a stub?
      (8) *Sibling divergence* — where N implementations of one trait exist, which one is
      the odd one out and why?
- [x] Task 0.6. Establish the **no-fix discipline**: during Stages 1–2 the working tree
      stays clean apart from `docs/audit/`. If a one-line obvious bug is spotted, it is
      still recorded as a finding, not fixed. Rationale: mixing fixes into the audit
      destroys the frozen anchor and makes the ledger un-reviewable.

### Phase 1 — Mechanical sweeps (cheap, whole-repo, run once, re-run at the end)

Each sweep produces findings directly *and* a risk map that reprioritises Stage 2.

- [x] Task 1.1. **Coverage map.** Produce a per-crate, per-file line/region coverage
      report and record the baseline in the ledger. There is currently **no coverage
      signal at all** despite 201 of 272 files carrying tests. Rationale: this converts
      "where are the holes" from a hypothesis into a map, and it is the single cheapest
      way to reorder Stage 2 toward genuinely unexercised code.
- [x] Task 1.2. **Dependency-policy gap.** Run the `bans` check that CI configures but
      never executes (CI runs only `licenses advisories sources`), and enumerate
      duplicate crate versions. Rationale: duplicate-version drift works directly against
      the project's stated top priority, binary size, and is currently unmonitored.
- [x] Task 1.3. **Advisory-ignore expiry review.** For each of the 14 documented ignores
      in `deny.toml`, confirm the stated exit condition still holds (GTK3 stack still
      never compiled in; `quick-xml` still build-time only; `ttf-parser` still without a
      replacement). Record any whose justification has lapsed. Do not re-argue the ones
      that hold.
- [x] Task 1.4. **`unsafe` inventory.** Enumerate every `unsafe` block across the 28
      files that contain one; for each, record whether a safety contract is documented
      and whether the caller can violate it. Note that 18 of 19 crates carry no
      crate-root `unsafe_code` policy while `fono-http` demonstrates the pattern.
      Rationale: this is the highest-consequence, least-governed surface in the repo.
- [x] Task 1.5. **Lint-escape inventory.** Catalogue every file-level `#![allow(...)]`
      (12 sites, concentrated in the overlay renderer and local-inference paths) and
      every inline `#[allow]`, and judge whether each is still load-bearing or has
      outlived the code it was added for.
- [x] Task 1.6. **Feature-matrix validity.** Determine which `--features` combinations
      are actually compiled by any gate, and identify combinations that are reachable by
      users but never built or tested. Rationale: feature-gating is the architectural
      spine of this project; an unbuilt combination is an unnoticed breakage waiting for
      a user.
- [x] Task 1.7. **Dead and unreachable code.** Identify exported items with no in-repo
      caller, `#[allow(dead_code)]` sites, and config keys that nothing reads. Every
      such item is both a size cost and a maintenance cost.
- [x] Task 1.8. **Ignored-test census.** Classify all 24 `#[ignore]` tests by *why* they
      are ignored (needs model, needs network, needs host) and flag any whose stated
      precondition is now satisfiable in CI. Note the standing exception: only the
      `fono-bench` latency group is actually run.
- [x] Task 1.9. **Panic-surface sweep.** Enumerate `unwrap`/`expect`/`panic!`/indexing
      /integer-division sites in non-test code and separate "provably infallible here"
      from "reachable with hostile or merely unusual input".
- [x] Task 1.10. **Gate-integrity review.** Audit the gates themselves as code: the
      SPDX header rule is enforced by review only despite `tests/check.sh` already
      demonstrating the exact enforcement pattern; CI path filters skip all of `docs/**`,
      yet `docs/bench/baseline-*.json` lives there, so editing a baseline does not re-run
      the gate that consumes it; the criterion bench is compiled but never executed, so
      there is no perf-regression signal; `cargo-bloat` reports are produced every run
      and may never be read. Record each as a finding with a suggested direction.
- [x] Task 1.11. **Reprioritise.** Fold the sweep results back into the unit ordering
      above and record the revised order with a one-line rationale per change.

### Phase 2 — Unit-by-unit reading

Repeat the identical sub-recipe for each unit A→N (or the reprioritised order). Each unit
is a self-contained work item that can be started and finished in one session.

- [x] Task 2.1. **Unit A — Session orchestration** (`crates/fono/src/session.rs`).
      Apply all eight lenses. Specific focus: map the exact ownership boundary between
      orchestrator state and the daemon's shared config; justify every mix of
      `std::sync::Mutex`/`RwLock` with `tokio::sync` in one type; check each of the 43
      spawn sites for a shutdown path; verify the raw `JoinHandle` is joined on every
      exit route.
- [x] Task 2.2. **Unit B — Daemon event loop** (`crates/fono/src/daemon.rs`).
      Focus: event ordering and dropped-event behaviour when a consumer is slow; what
      happens when tray, IPC, or overlay is absent or fails to start; re-entrancy when a
      hotkey fires during processing.
- [x] Task 2.3. **Unit C — Assistant runtime** (`assistant.rs`, `wake.rs`). Focus: draw
      the task topology explicitly (66 shared-state constructs); streaming-token
      cancellation mid-generation; tool-call error propagation; wake-word and
      push-to-talk interaction.
- [x] Task 2.4. **Unit D — State machines** (`fono-hotkey`, overlay state enum). Focus:
      exhaustiveness of transitions; whether the three pipeline guards are provably
      mutually exclusive; the three `Arc<AtomicBool>` used as a lock-free channel between
      the listener thread and the silence-watch task — memory ordering and lost-update risk.
- [x] Task 2.5. **Unit E — Audio real-time path** (`fono-audio`). Focus: buffer
      management and drop/overrun handling; resampling correctness at boundaries;
      VAD/silence-watch thresholds and hysteresis; the `fono-audio → fono-download` edge
      (confirm model fetching cannot occur on the capture hot path); speaker-verification
      DSP against the front-end parity spec recorded in `AGENTS.md`.
- [x] Task 2.6. **Unit F — Untrusted input decoders.** Focus: length-prefix handling and
      allocation bounds; truncated, oversized, and adversarial frames; partial reads;
      protocol state confusion. **Deliverable in addition to findings:** a written
      assessment of whether fuzz targets are warranted here and roughly what they would
      cost, since this is the only surface consuming off-host bytes.
- [x] Task 2.7. **Unit G — Network servers** (`fono-net`). Focus: auth enforcement on
      every route and every server; request-size and timeout limits; concurrent-client
      isolation; mDNS advertisement content vs. privacy expectations; bind-address defaults.
- [x] Task 2.8. **Unit H — FFI and `unsafe`.** Focus: for each site, state the invariant
      the `unsafe` relies on and whether any safe caller can break it; null/lifetime/
      alignment assumptions across the C boundary; error handling when a `dlopen`ed
      symbol is missing; behaviour on platforms where the probe fails.
- [x] Task 2.9. **Unit I — Local inference backends.** Focus: model handle lifetime and
      double-free/use-after-free potential across the three near-duplicate
      `llama_local.rs`/`whisper_local.rs` implementations; cancellation of a blocking
      generation; the `significant_drop_tightening` allows; and the **open Windows
      `0xc0000374` heap corruption** in `local_backends_coexist` — determine whether it is
      reproducible under Linux with the Vulkan feature, and record the analysis whether
      or not it reproduces.
- [x] Task 2.10. **Unit J — Config, paths, persistence.** Focus: schema migration and
      forward/backward compatibility; behaviour on malformed or partially-written config;
      secret storage and whether secrets can reach logs, history, crash output, or the web
      settings UI; SQLite transaction boundaries and concurrent daemon/CLI access;
      path handling on all three platforms.
- [x] Task 2.11. **Unit K — Cloud provider backends.** Audit primarily for **sibling
      divergence**: build a matrix of the N backends × (retry, timeout, streaming
      cancellation, error mapping, rate-limit handling, request-id capture, redaction) and
      investigate every cell that differs from its row-mates. Rationale: these are
      shape-identical by design, so a divergence is either an undocumented provider quirk
      or a bug — and the matrix finds both cheaply.
- [x] Task 2.12. **Unit L — Injection and platform integration.** Focus: clipboard
      save/restore correctness under failure and concurrent modification; behaviour when
      the target window changes mid-injection; the five backends' divergence; the
      app-context classifier's privacy posture against its ADR.
- [x] Task 2.13. **Unit M — Rendering and overlay.** Focus: whether the blanket
      file-level allows hide real defects; per-frame allocation on the render path;
      backend-selection fallback when a compositor protocol is unavailable; window
      lifetime and teardown on all four backends.
- [x] Task 2.14. **Unit N — Update, download, bench, CLI surface.** Focus: atomic binary
      replace — interrupted-update and partial-write behaviour, signature/SHA verification
      ordering, downgrade and rollback; Range-resume correctness against a truncated or
      changed remote; wizard failure modes on first run (highest blast radius for new
      users); CLI argument validation.
- [x] Task 2.15. **Cross-unit synthesis.** After all units, identify findings that recur
      across three or more units — these are systemic and are worth a single structural
      recommendation rather than N point fixes. Expected candidates based on the survey:
      the `std`/`tokio` sync-primitive mix, the absent crate-root lint policy, and
      duplicated provider-backend logic.

### Phase 3 — Triage and handoff (still no code changes)

- [x] Task 3.1. **De-duplicate and merge** the ledger; collapse near-identical findings
      into one with multiple locations.
- [x] Task 3.2. **Verify S1 and S2 findings.** For every finding at S1 or S2, either
      produce a concrete reproduction (a failing test written but *not committed*, a
      command, or an execution trace) or downgrade it and mark it explicitly
      "theory-only". Rationale: this is the single most important quality control on the
      whole programme — an unverified S1 wastes Stage 3 effort and erodes trust in the
      ledger.
- [x] Task 3.3. **Rank for Stage 3**: sort by severity, then by fix cost, then by blast
      radius. Explicitly flag any suggested fix that would introduce a dependency new to
      `Cargo.lock`, with its estimated binary-size impact, since that needs separate
      sign-off.
- [ ] Task 3.4. **Present the ranked ledger for approval.** The user approves, defers, or
      rejects per finding ID. Nothing is fixed without an ID-level approval.
- [x] Task 3.5. **Stage 3 execution rules** (recorded now, executed later, by an
      implementation agent — this agent does not write code): one approved finding or one
      tight cluster per commit; every commit cites its finding IDs; the full pre-commit
      gate plus the size-budget gate runs before each commit; the ledger row is marked
      resolved with its commit SHA. Where a fix is behavioural, a regression test lands in
      the same commit.

---

## Verification Criteria

- Every one of the 14 units has a ledger section, and no unit is closed without a written
  statement of what was examined and what was deliberately not examined.
- Every finding carries all required fields; no finding cites a location without
  `file:line`, and every citation resolves at the anchor SHA.
- All eight lenses are explicitly answered per unit — "no findings under this lens" is an
  acceptable and expected answer, silence is not.
- Every S1/S2 finding is either reproduced or explicitly downgraded to theory-only
  (Task 3.2), with zero unverified S1s remaining.
- Coverage baseline is recorded, and every unit's section notes its own coverage figure.
- The working tree contains no changes outside `docs/audit/` at the end of Stage 2.
- The final ledger is ranked and each entry has an approve/defer/reject disposition.

---

## Potential Risks and Mitigations

1. **Volume collapse — the audit produces 300 undifferentiated notes and stalls.**
   Mitigation: the severity rubric (Task 0.4) and the mandatory Stage-3 verification step
   (Task 3.2) force triage *before* the list is presented. S5 findings are batched into a
   single per-unit paragraph rather than individual entries.

2. **False positives from static reading — a "bug" that is actually handled upstream.**
   Mitigation: the mandatory Confidence field, and the rule that S1/S2 must be reproduced
   or downgraded. Reading a 6,800-line file in isolation reliably produces these; the
   verification gate is the answer, not more careful reading.

3. **The codebase moves under the audit, invalidating citations.**
   Mitigation: the frozen anchor SHA (Task 0.1) plus the no-fix discipline (Task 0.6). If
   development must continue in parallel, re-anchor at the start of each session and note
   the drift rather than silently reading a different tree.

4. **Scope creep into re-litigating settled ADR decisions.**
   Mitigation: the explicit "verify exit conditions, don't reopen decisions" boundary.
   A finding that contradicts an ADR must say which ADR and argue against it on new
   evidence, or it is out of scope.

5. **Suggested improvements that grow the binary.**
   Mitigation: the mandatory binary-size-impact field, and the Task 3.3 flag for any fix
   implying a dependency new to `Cargo.lock`. Size is the project's stated top priority;
   a suggestion that ignores it is not actionable.

6. **The mechanical sweeps consume the whole budget and the reading never happens.**
   Mitigation: Phase 1 is deliberately tool-driven and time-boxed to one pass; its
   *purpose* is to reprioritise Phase 2, not to be the audit. If a sweep needs new
   tooling, that is itself a finding, not a prerequisite.

7. **Platform-specific code is audited by reading only and defects go unfound.**
   Mitigation: state this limitation per unit (H, L, M, and the Windows half of I), and
   record "unverifiable without host" findings in a separate ledger section so they are
   not mistaken for cleared code.

---

## Alternative Approaches

1. **Dependency-order sweep (tier 0 → tier 5).** Audit the leaves first, then work up.
   *Trade-off:* better for building understanding and for the auditor's confidence in each
   layer, but it spends the earliest and freshest effort on `fono-download` (165 lines)
   and `fono-ipc` (410) — the four lowest-risk crates — and reaches the 6,800-line
   orchestrator last. Rejected as the primary order; the risk-first order above is
   preferred, with the leaf crates batched into a single short unit if desired.

2. **Lens-major instead of unit-major** — sweep the whole repo once per lens
   (all concurrency everywhere, then all error paths everywhere, …).
   *Trade-off:* excellent at catching systemic patterns and sibling divergence, and it
   naturally produces the Task 2.15 synthesis for free; but it requires holding the whole
   codebase in context at once and makes partial progress hard to bank between sessions.
   Best used as a *supplement* — Unit K (cloud backends) already adopts this style
   deliberately, and Task 2.15 recovers the rest.

3. **Tool-heavy first: add coverage, fuzzing, property tests and miri, then let the tools
   find the bugs.** *Trade-off:* higher long-term value and produces durable gates rather
   than a one-off document, but it is a build project, not an audit, and it front-loads
   weeks of infrastructure before the first finding. The plan above takes the middle path:
   Phase 1 uses only tooling that requires no code changes, and where deeper tooling is
   warranted (fuzzing on Unit F) it produces a *sized recommendation* rather than an
   implementation.

4. **Outsource breadth to parallel research passes, keep depth manual.** Run mechanical
   and pattern-matching investigation across many files concurrently, and reserve careful
   line-by-line reading for the S1/S2 candidates that surfaces. *Trade-off:* substantially
   faster wall-clock coverage of 156k lines, at the cost of a higher false-positive rate —
   which Task 3.2 is already designed to absorb. Recommended as the execution style for
   Phase 1 and for the lower-risk units.
