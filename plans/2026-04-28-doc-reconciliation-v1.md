# Doc / Plan Reconciliation Pass

## Objective

Bring the plan tree, ADR set, and `docs/status.md` back into agreement
with what is actually shipped on `main` (HEAD `4517133`). Today every
"Recommended next session" hint in `status.md` and every open
checkbox across `plans/` and `docs/plans/` must be cross-checked
against the codebase before it can be trusted; this wave fixes that
once and lets the next four waves (`Wave 2-5` in the strategic
revision conversation) execute without re-auditing.

Concretely:

1. Confirm the `crates/fono/tests/pipeline.rs` "broken on main" claim
   in `docs/status.md:50-52` is stale (signatures align in current
   source) by actually running the test, and either delete the claim
   or fix what is genuinely broken.
2. Tick checkboxes and write `docs/status.md` entries for the four
   commits whose work shipped without being reflected in the plan
   tree: `3e2c742` (self-update), `b6596c0`/`7db29b5` (equivalence
   accuracy gate + multilingual fixtures), `7bea0a9` (R3.1 in-wizard
   latency probe + a *partial* R5.1 CI bench gate that the commit
   subject overstates).
3. Mark the three obsolete plans (`candle-backend-benchmark`,
   `llama-dynamic-link-sota`, `shared-ggml-static-binary`) as
   superseded by the `--allow-multiple-definition` link trick already
   live in `.cargo/config.toml`.
4. Fill the ADR numbering gaps (`0005-0008`, `0010-0014`) with
   reconstructed records pointing at the commits and status entries
   where each decision was actually made; add new ADRs `0017`
   (auto-translation forward-reference, even though the feature is
   not started, so the next wave has a stable home), `0018` (the
   `--allow-multiple-definition` link trick), and `0019` (platform
   scope: Linux-multi-package for v0.x).
5. Update `docs/plans/2026-04-25-fono-roadmap-v2.md` Tier-1
   checkboxes against reality (most ticked, R5.1 demoted to a
   compile-sanity caveat with a real-fixture follow-up explicitly
   carried forward).
6. Update `docs/status.md` Active plans table and Phase progress
   table to reflect the closed/superseded/in-progress state and
   write a new "Recommended next session" pointing to the actually
   open work (Wave 2 of the revised plan = closing out the half-done
   self-update + accuracy-gate plans).

This wave changes **only documentation, plan files, and ADRs**. No
Rust source is modified. No new binary capability ships. No tests are
removed; if Task 1 reveals a real test failure, that fix is captured
as a follow-up plan rather than blocking this reconciliation.

## Background

`sage` audit on 2026-04-28 (see the immediately-prior conversation
turn in this session) produced concrete file:line evidence for each
of the gaps above. Highlights:

- `crates/fono-update/` is a full ~660-line crate (commit `3e2c742`,
  2026-04-22) implementing the self-update plan at ~85% — tray
  entry, `fono update` CLI, atomic replace, rollback `.bak`,
  `FONO_NO_UPDATE_CHECK` env var. `plans/2026-04-27-fono-self-update-v1.md`
  has every box still empty.
- `crates/fono-bench/src/equivalence.rs:113-114` populates
  `Metrics.stt_accuracy_levenshtein`; `crates/fono-bench/src/bin/fono-bench.rs:339`
  short-circuits non-English fixtures on English-only models;
  `print_table` shows the `acc` column. The plan
  `plans/2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md`
  has every box still empty even though commits `b6596c0` and
  `7db29b5` shipped roughly half of it.
- `.cargo/config.toml:21-28` carries
  `link-arg=-Wl,--allow-multiple-definition` for `linux-gnu`,
  `linux-musl`, and `windows-gnu` targets. This is the path that
  status.md:276-310 documents and that the three obsolete plans were
  written to address before it landed. None of those plans was ever
  executed.
- `docs/decisions/` contains files `0001-0004`, `0009`, `0015`,
  `0016` only. `docs/status.md` repeatedly references ADRs `0007`
  (musl), `0008` (llama-local-deferred), `0014` (equivalence
  harness), and others that were lost in a `filter-branch: rewrite`
  earlier in the project's history. The numbering gap signals
  decisions made without surviving rationale.
- `git tag` lists `v0.1.0`, `v0.1.1`, `v0.2.0`, `v0.2.1`. The
  `roadmap-v2.md` R4.4 checkbox claiming "tag v0.1.0" is open is
  three tags behind reality.
- `.github/workflows/ci.yml:64-68` runs `cargo bench --no-run` (a
  *compile* sanity check) but no real-fixture run-and-compare gate.
  `7bea0a9` advertised this as "R5.1 CI bench gate". The bench gate
  the roadmap actually wants — re-run the equivalence harness on
  every PR and fail on regression — is a Wave 2 follow-up, not
  shipped.

The strategic revision turn that produced this plan is preserved in
the agent session log and contains the full audit table.

## Implementation Plan

### Phase 1 — Verification only (no edits yet)

- [ ] Task 1. Run `cargo test --workspace --lib --tests` and capture
  full pass/fail output. The expected outcome is **all green** based
  on the `sage` static read of `crates/fono/src/session.rs:140-142`
  vs `crates/fono/tests/pipeline.rs:54-58`. If a real failure
  surfaces, **stop this plan**, capture the failure in a one-page
  follow-up plan
  (`plans/2026-04-28-pipeline-test-fix-v1.md`), and return to the
  user — do not paper over a real test break with a doc edit.
  Rationale: the reconciliation premise is "the only break is the
  doc"; verify before propagating that premise.

- [ ] Task 2. Run `cargo build --workspace` and `cargo clippy
  --workspace --no-deps -- -D warnings` from a clean tree to confirm
  baseline health before touching anything. Capture output. If
  either fails, treat the same way as Task 1.

### Phase 2 — Close out the four undocumented commits

- [ ] Task 3. Add a new dated entry to the **top** of
  `docs/status.md` (above the existing 2026-04-28 entries) titled
  `## 2026-04-28 — Doc reconciliation pass`. Body summarises (a)
  that `crates/fono/tests/pipeline.rs` actually compiles and passes
  on HEAD (citing the Task 1 result), (b) that self-update / accuracy
  gate / wizard latency probe shipped without being reflected in the
  plan tree, and (c) that this pass ticks the relevant boxes and
  promotes the three superseded plans to `plans/closed/`. Link the
  exact commit SHAs (`3e2c742`, `b6596c0`, `7db29b5`, `7bea0a9`).

- [ ] Task 4. **Self-update plan close-out** —
  `plans/2026-04-27-fono-self-update-v1.md`:
  - Tick Tasks 1, 2, 3 (Phase 1 foundations) — evidence: every
    symbol named in the plan exists in `crates/fono-update/src/lib.rs`
    (`UpdateInfo` `:31-107`, `UpdateStatus`, `Channel` `:38-59`,
    `is_newer` `:118-127`, `release` module).
  - Tick Tasks 4, 5, 6, 7 (Phase 2 background checker) — evidence:
    `crates/fono/src/daemon.rs:145-185` spawns the checker and
    persists `update.json`; `[update]` config is wired in
    `crates/fono-core/src/config.rs:47, 70`; `FONO_NO_UPDATE_CHECK`
    honoured at `crates/fono-update/src/lib.rs:267`.
  - Tick Tasks 8, 9, 10 (Phase 3 tray) — evidence:
    `crates/fono-tray/src/lib.rs:78,487-494`; daemon hook at
    `crates/fono/src/daemon.rs:476, 514, 1195-1213`.
  - Tick Tasks 11, 13, 14 (Phase 4 atomic replace + restart) —
    evidence: `apply_update` at `crates/fono-update/src/lib.rs:381-477`,
    `restart_in_place` `:507-529`.
  - Tick Task 15 (rollback `.bak`) **partially**: `.bak` sidecar
    exists `:455-463`, but the `--self-check` smoke flag does not.
    Add an inline `(partial — `.bak` only; smoke `--self-check`
    deferred)` annotation and carry the smoke flag forward in the
    "Open follow-ups" section at the bottom of the plan file.
  - Tick Tasks 17, 18, 19 (Phase 5 CLI + Phase 6 package detection).
  - **Leave open** Tasks 12 (per-asset `.sha256` sidecar
    verification), 16 (`--bin-dir` flag), 20 (release workflow emits
    `.sha256` per asset), 21 (unit + integration tests), 22 (manual
    QA checklist `docs/dev/update-qa.md`). Add a new "## Status"
    header at the top of the plan file:
    `Status: ~85% landed in 3e2c742; remaining work tracked as Wave 2
    Task 8 of plans/2026-04-28-doc-reconciliation-v1.md.`

- [ ] Task 5. **Equivalence accuracy gate plan close-out** —
  `plans/2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md`:
  - Tick Task 7 (capability short-circuit before STT) **partially**
    with annotation: implemented as inline boolean
    `english_only = args.stt == "local" && args.model.ends_with(".en")`
    at `crates/fono-bench/src/bin/fono-bench.rs:339`, not as a
    typed `ModelCapabilities`. Verdict shape (`Verdict::Skipped`
    with note) matches plan intent.
  - Tick Task 8 (`Metrics.stt_accuracy_levenshtein: Option<f32>`) —
    evidence: `crates/fono-bench/src/equivalence.rs:113-114`,
    populated at `:527`.
  - Tick Task 12 (`acc` column in `print_table`) — evidence:
    `crates/fono-bench/src/bin/fono-bench.rs:527`.
  - Tick Task 17 (`tests/bench.sh` legend updated, multilingual
    fixtures in tree) — evidence: commit `b6596c0`.
  - **Leave open** Tasks 1-6 (typed `ModelCapabilities` +
    `accuracy_threshold` + `requires_multilingual` + threshold
    alias), 9-10 (combined verdict + sub-verdict notes), 11 (caps
    resolved once + threaded through), 13 (`EquivalenceReport.model_capabilities`
    field), 14 (overall verdict treats capability skips as inert —
    incidentally true today but not by typed contract), 15 (mock-STT
    capability-skip test + two-gate verdict tests), 16 (integration
    smoke), 18 (status.md entry — done by this plan in Task 3).
  - Add a `## Status` header:
    `Status: ~50% landed in b6596c0/7db29b5 as inline behaviour; remaining
    typed-API refactor tracked as Wave 2 Task 7 of plans/2026-04-28-doc-reconciliation-v1.md.`

- [ ] Task 6. **Roadmap v2 reconciliation** —
  `docs/plans/2026-04-25-fono-roadmap-v2.md` Tier-1 checkboxes:
  - Tick R2.1 (tray STT/LLM submenus) — `crates/fono-tray/src/lib.rs:418-450`,
    documented `docs/status.md:458-465`.
  - Tick R3.1 (in-wizard latency probe) — commit `7bea0a9`,
    `crates/fono/src/wizard.rs:72,720,725`.
  - Tick R3.2 (cloud key validation in wizard) —
    `crates/fono/src/wizard.rs:532-552`, `docs/status.md:445-449`.
  - Tick R3.3 (mixed pipeline) — `crates/fono/src/wizard.rs:54-55,
    257-315`, `docs/status.md:441-443`.
  - Tick R4.1 (README first-run snippet aligned).
  - Tick R4.2 (`docs/inject.md`) — `docs/status.md:450-451`.
  - Tick R4.3 (`docs/troubleshooting.md`) — `docs/status.md:452-454`.
  - Tick R4.4 (`git tag v0.1.0`) — and append a one-liner: "Tags
    `v0.1.0`, `v0.1.1`, `v0.2.0`, `v0.2.1` exist; current tip is
    `v0.2.1`."
  - **Demote R5.1** to:
    `R5.1 (partial). Compile-sanity wired in .github/workflows/ci.yml:64-68
    (cargo bench --no-run); real-fixture equivalence-harness gate is
    Wave 2 Task 9 of plans/2026-04-28-doc-reconciliation-v1.md.`
  - **Leave open** R1.1, R1.2, R1.3 (real-machine smoke runs require
    physical access), R2.2 (Edit last), R2.3 (cross-DE badge
    verification), R5.2 (baseline JSON commit).

### Phase 3 — Close obsolete plans

- [ ] Task 7. Create `plans/closed/` directory.

- [ ] Task 8. Move the three superseded plans into `plans/closed/`
  using `git mv` so history is preserved:
  - `plans/2026-04-27-candle-backend-benchmark-v1.md` →
    `plans/closed/2026-04-27-candle-backend-benchmark-v1.md`
  - `plans/2026-04-27-llama-dynamic-link-sota-v1.md` →
    `plans/closed/2026-04-27-llama-dynamic-link-sota-v1.md`
  - `plans/2026-04-27-shared-ggml-static-binary-v1.md` →
    `plans/closed/2026-04-27-shared-ggml-static-binary-v1.md`

- [ ] Task 9. Prepend a `## Status: Superseded` block to each of the
  three closed plans pointing at:
  - the actual fix (`.cargo/config.toml:21-28` —
    `link-arg=-Wl,--allow-multiple-definition`),
  - the documenting status entry (`docs/status.md:276-310`),
  - the new ADR `0018-ggml-link-trick.md` written in Task 13 below.
  Body of the status block: 4-5 lines explaining why the plan was
  obsoleted before execution, and naming the rollback path (plan
  H — shared ggml — is the documented escape hatch if the link
  trick fails on a future linker).

- [ ] Task 10. Create `plans/closed/README.md` (~15 lines)
  describing the directory's purpose: closed-but-preserved plans,
  each carrying a `Status: Superseded`, `Status: Abandoned`, or
  `Status: Completed` header explaining why it left active rotation.
  The directory is **not** garbage; it is the project's record of
  decisions-not-taken.

### Phase 4 — ADR backfill

- [ ] Task 11. Inventory ADR references currently dangling in
  `docs/`. Search (`fs_search` regex `0\\d\\d\\d-[a-z-]+\\.md` under
  `docs/`) to find every ADR cited in status / plans / READMEs.
  Cross-reference against `docs/decisions/` filesystem listing to
  build the gap table. Expected gaps from the audit: `0005`, `0006`,
  `0007`, `0008`, `0010`, `0011`, `0012`, `0013`, `0014`, plus new
  numbers `0017`, `0018`, `0019`.

- [ ] Task 12. Author reconstructed ADRs `0005`-`0008`, `0010`-`0014`.
  Each file:
  - Filename: best guess from status.md references, e.g.
    `0005-static-binary-distribution.md`,
    `0006-xdg-paths.md`, `0007-musl-vs-glibc.md`,
    `0008-llama-local-deferred.md`,
    `0010-streaming-runtime-toggle.md`,
    `0011-overlay-in-process-vs-subprocess.md`,
    `0012-budget-controller.md`,
    `0013-equivalence-on-committed-only.md`,
    `0014-equivalence-harness.md`.
    Confirm exact filenames during execution by grepping status.md
    and the v6/v7 interactive plans for the cited ADR numbers; if
    a different name is in use, follow that.
  - Header: `# ADR NNNN — <title>` followed by
    `Status: Reconstructed (original lost in filter-branch rewrite;
    rationale recovered from docs/status.md and plan history,
    YYYY-MM-DD).`
  - Body: 1-2 paragraphs of context, the decision, and the
    consequences. Source material is `docs/status.md` plus the
    relevant plan file. Each ADR is short — a stub is acceptable
    when the original rationale is genuinely lost; do **not**
    fabricate detail. When the genuine rationale cannot be
    reconstructed, write `(Rationale not recovered; this stub
    exists to fill the numbering gap and link the relevant
    surviving artefacts.)` and link the artefacts.

- [ ] Task 13. Author `docs/decisions/0018-ggml-link-trick.md`
  capturing the active decision today: `--allow-multiple-definition`
  as the static-binary path for whisper-rs + llama-cpp-2
  coexistence. Include:
  - Decision statement (single binary; both ggml copies linked;
    duplicates discarded; one set kept).
  - Verification (`nm target/release/fono | grep ' [Tt] ggml_init$'`
    → exactly one entry per `docs/status.md:286-289`).
  - Trade-offs (ABI compatibility burden if the two crates' ggml
    drift; both originate from `ggerganov` upstream today).
  - Rollback path (plan H —
    `plans/closed/2026-04-27-shared-ggml-static-binary-v1.md` — is
    the documented fallback if the link trick fails on a future
    linker; lld and ld64 currently behave the same as bfd/gold for
    this option).
  - Pointer to the smoke test
    `crates/fono/tests/local_backends_coexist.rs`.

- [ ] Task 14. Author `docs/decisions/0017-auto-translation.md` as
  a **forward-reference** ADR. Body: "Status: Pending — the auto-
  translation feature design is captured in
  `plans/2026-04-28-fono-auto-translation-v1.md`. This ADR exists
  to reserve the number and provide a stable home for the decision
  record once the plan begins execution. The decision will be
  authored as part of Wave 4 Task 15 of the revised strategic plan."
  This avoids a future numbering scramble when translation lands.

- [ ] Task 15. Author `docs/decisions/0019-platform-scope.md`
  documenting the v0.x scope decision: Linux-multi-package release
  matrix (bare ELF + .deb + .pkg.tar.zst + .txz + .lzm), no macOS /
  Windows release artefacts. Cite `.github/workflows/release.yml`
  (5 jobs, all Linux-targeted), explain the user-base rationale
  (Linux-first dictation tool replacing Tambourine + OpenWhispr on
  light distros per `AGENTS.md`), document that the original
  Phase 9 "six artifacts" target (`docs/plans/2026-04-24-fono-design-v1.md:530-531`)
  is amended to "five Linux artifacts" for v0.x, and that
  cross-platform release jobs are revisited as a v1.0 concern.

### Phase 5 — Status log + handoff

- [ ] Task 16. Update `docs/status.md` Active plans table (line
  ~536-543) to reflect post-reconciliation state:
  - Add a row for `plans/2026-04-27-fono-self-update-v1.md` —
    `~85% landed in 3e2c742; finishing pass tracked as Wave 2 Task 8`.
  - Add a row for `plans/2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md`
    — `~50% landed in b6596c0/7db29b5; typed-API refactor tracked
    as Wave 2 Task 7`.
  - Add a row for `plans/2026-04-28-fono-auto-translation-v1.md` —
    `Not started (Wave 4 of revised strategic plan)`.
  - Add a row for the three superseded plans now in
    `plans/closed/` — single combined row.

- [ ] Task 17. Replace the existing "Recommended next session"
  block in `docs/status.md` (currently lines ~764-774) with one
  pointing at Wave 2 of the revised strategic plan. Body:

  > Recommended next session: execute **Wave 2** of the revised
  > strategic plan (this conversation, 2026-04-28). Wave 2 closes
  > out the half-shipped self-update and accuracy-gate plans, and
  > tightens the CI bench gate from compile-sanity to a real-fixture
  > equivalence run. Concretely:
  >
  > 1. Equivalence accuracy gate close-out — typed `ModelCapabilities`
  >    in `crates/fono-bench/src/capabilities.rs`, `accuracy_threshold` /
  >    `requires_multilingual` on `ManifestFixture`, `model_capabilities`
  >    in `EquivalenceReport`, mock-STT capability-skip test.
  > 2. Self-update finishing pass — per-asset `.sha256` sidecar
  >    verification, `--bin-dir` CLI flag, `docs/dev/update-qa.md`
  >    checklist, release workflow emits `.sha256` per asset.
  > 3. Real-fixture CI gate — replace `cargo bench --no-run` in
  >    `.github/workflows/ci.yml:64-68` with a `fono-bench
  >    equivalence` run against `tests/fixtures/equivalence/manifest.toml`
  >    and commit `docs/bench/baseline-local-comfortable.json` as the
  >    PR comparison anchor (R5.2).

- [ ] Task 18. Verification gate before commit:
  - `cargo build --workspace` clean.
  - `cargo test --workspace --lib --tests` clean (re-run to confirm
    Phase 1 hasn't regressed; no source touched in this plan, so
    failure indicates a pre-existing flake).
  - `git status` lists only:
    - `docs/status.md` (modified, two new entries + table updates +
      Recommended next session block).
    - `docs/decisions/0005-*.md` … `0019-*.md` (new files; eight to
      twelve total depending on Task 11 inventory).
    - `plans/2026-04-27-fono-self-update-v1.md` (modified —
      checkboxes + status header).
    - `plans/2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md`
      (modified — checkboxes + status header).
    - `plans/2026-04-28-doc-reconciliation-v1.md` (this file, new).
    - `docs/plans/2026-04-25-fono-roadmap-v2.md` (modified —
      checkboxes + R5.1 demotion).
    - `plans/closed/` (new directory with three moved plans + README).
  - Zero `.rs`, `.toml`, `Cargo.lock`, `*.yml`, or `*.json` changes.
  - `cargo clippy --workspace --no-deps -- -D warnings` clean (no
    source change should not regress clippy; defensive re-run).

### Phase 6 — Commit + handoff

- [ ] Task 19. Stage and commit in three logical chunks (each
  DCO-signed per `AGENTS.md`):
  1. `docs(plans): close out self-update + equivalence accuracy
     gate; reconcile roadmap-v2` — touches the three modified plan
     files only.
  2. `docs(plans): supersede candle / dynamic-link / shared-ggml
     plans (mooted by --allow-multiple-definition)` — the `git mv`
     into `plans/closed/`, the `Status: Superseded` headers, and
     `plans/closed/README.md`.
  3. `docs(decisions): backfill ADRs 0005-0008, 0010-0014; add 0017
     (translation forward-ref), 0018 (ggml link trick), 0019
     (platform scope); status reconciliation` — all new ADR files
     plus the `docs/status.md` updates plus this plan file.

  Each commit message body cites the audit conversation and the
  specific evidence file:line ranges. Subject lines stay ≤ 72 chars.

- [ ] Task 20. Final summary report to the invoking agent (or
  user): list of commits created (with subjects + SHAs), list of
  files changed in each, confirmation that Phase 1 verification
  passed, and a one-paragraph "what's still open" summary pointing
  at the new `docs/status.md` Recommended next session.

## Verification Criteria

- `cargo test --workspace --lib --tests` is green at start and end
  of the wave (Tasks 1, 18).
- `cargo build --workspace` and `cargo clippy --workspace --no-deps
  -- -D warnings` are clean at start and end (Tasks 2, 18).
- `docs/decisions/` lists files `0001` through `0019` with no gaps
  (Tasks 11-15). Each file has either an authentic record, a
  reconstructed-with-citation record, or — for genuinely lost
  rationale — an explicit "(Rationale not recovered; stub.)"
  acknowledgement.
- `plans/2026-04-27-fono-self-update-v1.md` and
  `plans/2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md`
  each carry an explicit `## Status: ~N% landed; remaining work
  tracked as <Wave 2 task pointer>` header at the top of the file
  (Tasks 4, 5).
- `plans/closed/` contains exactly three plan files plus a `README.md`
  (Tasks 8, 10).
- `docs/plans/2026-04-25-fono-roadmap-v2.md` Tier-1 reflects the
  Task 6 reality (most ticked, R5.1 demoted, R1/R2.2/R5.2 still
  open).
- `docs/status.md` has a new top-of-file entry `## 2026-04-28 — Doc
  reconciliation pass` (Task 3), an updated Active plans table
  (Task 16), and a new "Recommended next session" block pointing
  at Wave 2 (Task 17).
- `git diff` shows zero changes outside `docs/`, `plans/`, and
  `docs/decisions/`.
- Three DCO-signed commits land on `main` (Task 19), each with a
  body citing the supporting audit evidence.
- Every new `.md` file under `docs/decisions/` and the new file
  `plans/2026-04-28-doc-reconciliation-v1.md` itself omits the
  Rust SPDX header (markdown does not require it per `AGENTS.md`'s
  rule "Every Rust source file" — verify the rule's wording before
  committing; if the project convention is to add an HTML-comment
  SPDX line, follow that).

## Potential Risks and Mitigations

1. **Phase 1 surfaces a real test break.** The audit was static;
   `cargo test` may reveal something `sage` couldn't see.
   Mitigation: Task 1 explicitly stops the wave and files a
   one-page follow-up plan rather than papering over a real bug
   with doc edits.

2. **ADR filename guesses (Task 12) collide with what some surviving
   reference expects.** A status entry citing `0007-musl.md` would
   fail to find `0007-musl-vs-glibc.md`.
   Mitigation: Task 11 inventories *every* dangling ADR reference
   first; Task 12 picks filenames that match the references found,
   not aesthetic guesses.

3. **Three logical commits (Task 19) may be too granular for a
   single doc pass and produce noise in `git log`.**
   Mitigation: acceptable noise — three commits is well within the
   project's history density and each chunk is a different concern
   (plan-tree, supersession, ADRs+status). If the implementing
   agent finds a single combined commit cleaner, that is fine
   provided the message body still cites all three concerns.

4. **`docs/status.md` "Recommended next session" rewrite drifts from
   the conversation's strategic plan if the user changes priorities
   between this wave and Wave 2.**
   Mitigation: Task 17 phrases the recommendation as "execute Wave
   2 of the revised strategic plan (this conversation, 2026-04-28)"
   with an explicit date, so a future session reading status.md
   knows the recommendation is anchored to a specific decision and
   can re-evaluate.

5. **Reconstructed ADRs (Task 12) may codify wrong rationale.**
   Mitigation: each reconstructed ADR carries the explicit
   `Status: Reconstructed` header with date; the project's record
   honestly reflects the recovery, not a fabrication.

6. **Plan files moved into `plans/closed/` may break paths cited
   elsewhere in the docs.**
   Mitigation: after Task 8, run `fs_search` for the three plan
   filenames across the entire repo; update any surviving
   references to point at the new `plans/closed/` paths. Most
   likely affected: `docs/status.md`, `CHANGELOG.md`, the v1-v6
   interactive plan files.

7. **Task 18's "zero source changes" gate fails because clippy
   surfaces a pre-existing nursery lint flake on a fresh toolchain.**
   Mitigation: if clippy fails for a reason demonstrably unrelated
   to this plan's edits, document the lint output in the final
   summary report, do **not** fix it as part of this wave (would
   violate the doc-only scope), and file it as a one-line follow-up
   plan.

## Alternative Approaches

1. **Squash all six commits worth of work into one giant doc commit.**
   Trade-off: simpler `git log` line, but harder to revert any
   single concern (e.g. if a reconstructed ADR turns out wrong, you
   can't revert it without losing the plan reconciliation).
   Rejected — three commits is the right granularity.

2. **Skip ADR backfill (Phase 4) and just do checkbox / supersession
   work.** Trade-off: faster (one commit instead of three), but
   leaves the dangling ADR references in `docs/status.md` for the
   next session to trip over. Half-finishes the reconciliation.
   Rejected — the ADR backfill is the highest-leverage piece for
   future contributors.

3. **Delete superseded plans rather than moving to `plans/closed/`.**
   Trade-off: tidier `plans/` listing, but loses the institutional
   memory that those approaches were considered and rejected. Bad
   for future debugging if the link trick ever fails. Rejected.

4. **Open new GitHub issues for every still-open task instead of
   keeping them in plan files.** Trade-off: matches conventional
   OSS triage, but the project has been plan-driven via `plans/`
   and `docs/plans/` since Phase 0; switching tracking systems
   mid-project doubles the indirection. Rejected for this wave;
   could be a v1.0-era cleanup.

5. **Combine this plan with Wave 2 (the actual code follow-ups
   for self-update + accuracy gate) into one mega-wave.** Trade-off:
   one fewer plan file, but mixes pure-doc edits with cross-crate
   refactors and inflates per-commit blast radius. Rejected — the
   doc-only / source-touching split is exactly the seam this plan
   defends.
