# Fono Code Audit — Findings Ledger

**Status:** Stage 1 (sweeps) **complete** — 16 findings, `F-0001`–`F-0016`.
Stage 2 (per-unit reading) not started. **No source file has been modified.** Fixes
happen only in Stage 3, only for findings approved by ID.

## Audit anchor

| | |
|---|---|
| **Anchor commit** | `86a7a41f2be5dbe8a9d318fc049eb2eabb2a964b` |
| **Anchor date** | 2026-08-06 |
| **Anchor subject** | Release 0.18.1: see what the assistant keeps warm |
| **Tree state at kickoff** | clean (no tracked modifications) |
| **Audit opened** | 2026-08-07 |

Every `file:line` citation below resolves against the anchor commit. If `main`
advances during the audit, findings are **re-anchored** (citations updated, drift
noted) rather than re-derived.

**Drift during Stage 1.** `main` advanced to `c44124ce9cf71521d59531a28dfc6408e816ee5b`
over three commits (`127121e`, `ea055ab`, `c44124c`) while the sweeps ran. All three
touch `plans/` only — 7,985 deletions, 4 insertions, **no source, config, workflow or
test file changed**. Every citation in this ledger therefore resolves unchanged at both
commits, and no re-anchoring was needed.

## Severity rubric

| Level | Name | Definition |
|---|---|---|
| **S1** | Correctness-critical | Data loss, hang, crash, wrong output reaching the user, or secret leak. |
| **S2** | Behavioural defect | Wrong behaviour under a reachable but non-default condition. |
| **S3** | Latent risk | Works today, fragile under change — races, unchecked invariants, TOCTOU. |
| **S4** | False-confidence test | A test that passes while the mechanism it names is disarmed. |
| **S5** | Improvement | Clarity, duplication, dead code, ergonomics. Batched per unit, not itemised. |

**Confidence:** `high` (read the code and the callers, believe it) · `medium`
(believe it, have not traced every caller) · `low` (pattern-matched, needs a look).

## Finding record format

Every finding carries all of:

**ID** · **Unit** · **Severity** · **Confidence** · **Location** (`file:line`) ·
**Observation** · **Why it is wrong / suboptimal** · **Reproduction** (or the literal
word `theory-only`) · **Suggested direction** · **Blast radius** ·
**Binary-size impact** (only if the suggestion implies a dependency new to `Cargo.lock`).

IDs are stable and never reused: `F-<unit><nnn>`, e.g. `F-A007`. Sweep findings
(Phase 1, not tied to a review unit) use unit letter `0`, e.g. `F-0003`.

## Audit lenses

Applied to every review unit; "no findings under this lens" is a required answer,
silence is not.

1. **Cancellation & shutdown** — can every spawned task be stopped, and what happens to in-flight work?
2. **Error paths** — is every `?`/`unwrap`/`expect` on a path a user can reach, and what do they see?
3. **Concurrency** — lock ordering, locks held across `await`, `std::sync` inside async, atomics used as channels.
4. **Invariants** — what does this module assume that nothing enforces?
5. **Boundary values** — empty input, zero-length audio, unicode, very long text, clock going backwards.
6. **Resource lifetime** — files, sockets, model handles, temp files, on both success and failure.
7. **False-confidence tests** — does the test exercise the mechanism or a stub?
8. **Sibling divergence** — where N implementations of one trait exist, which is the odd one out, and why?

## No-fix discipline

During Stages 1 and 2 the working tree stays clean apart from `docs/audit/`. A
one-line obvious bug spotted mid-review is **recorded as a finding, not fixed** —
mixing fixes into the audit invalidates the anchor and makes the ledger
un-reviewable.

---

# Stage 1 — Mechanical sweeps

Tooling installed for this stage (outside the repo, `~/.cargo/bin`):
`cargo-llvm-cov 0.8.7`, `cargo-deny 0.20.2`. Coverage uses the system LLVM
(`/usr/bin/llvm-cov` 22.1.8) against rustc 1.96.0's LLVM 22.1.2 — same major,
compatible.

## Sweep index

| Task | Sweep | Status | Findings |
|---|---|---|---|
| 1.1 | Coverage map | see below | `F-0016`+ |
| 1.2 | Dependency-policy gap | done | `F-0002`, `F-0003` |
| 1.3 | Advisory-ignore expiry | done | `F-0005`, `F-0006`, `F-0007` |
| 1.4 | `unsafe` inventory | done | `F-0004` |
| 1.5 | Lint-escape inventory | done | `F-0014` |
| 1.6 | Feature-matrix validity | done | `F-0012` |
| 1.7 | Dead and unreachable code | done | `F-0015` |
| 1.8 | Ignored-test census | done | `F-0013` |
| 1.9 | Panic-surface sweep | done | map only, no finding |
| 1.10 | Gate-integrity review | done | `F-0001`, `F-0009`, `F-0010`, `F-0011` |
| — | (spotted during 1.7) | done | `F-0008` |

---

## F-0001 — The comment-hygiene gate misses every slice named with a letter

| | |
|---|---|
| **Unit** | 0 (sweep 1.10) |
| **Severity** | S4 — false-confidence gate |
| **Confidence** | high |
| **Location** | `tests/check.sh:107` |

**Observation.** The gate greps for `plans/[0-9]{4}-|[Ss]lice [0-9]|Task [0-9]|[Pp]lan v[0-9]`.
The codebase does not number its slices — it letters them. `Slice A`, `Slice B`,
`Slice D`, `Phase B` all pass the gate untouched. A scan at the anchor commit finds
roughly 30 surviving violations of the rule the gate exists to enforce, including
`crates/fono/src/live.rs:14`, `:121`, `:133`, `:255`, `:288`, `:433`, `:442`, `:597`;
`crates/fono/src/session.rs:663`, `:714`, `:4210`, `:4241`, `:4401`, `:4612`, `:4620`;
`crates/fono-core/src/budget.rs:18`, `:20`, `:50`; `crates/fono-bench/src/equivalence.rs:12`,
`:20`, `:33`, `:122`, `:315`; `crates/fono-stt/src/factory.rs:509`;
`crates/fono-audio/src/wake_registry.rs:475`.

**Why it is wrong.** `AGENTS.md` states the rule as permanent and absolute — "Same for
slice, phase, task and plan-version numbers". The gate implements a narrower rule than
the one written down, so the project believes it is enforced when it is not. Every one
of those comments points a future reader at a plan document that, by the project's own
policy, is a snapshot and not maintained truth.

**Reproduction.**
`git ls-files '*.rs' | xargs grep -nE '[Ss]lice [A-Z]\b|[Pp]hase [A-Z]\b'` — returns
~30 hits, while `tests/check.sh` exits 0.

**Suggested direction.** Widen the character class to `[0-9A-Z]` in the existing
pattern. Note this will fail the gate immediately on ~30 existing comments, so the
regex change and the comment cleanup are one unit of work. Beware two legitimate
collisions the widened pattern would catch: `crates/fono-core/src/brain_tap.rs:1004`
and `:1043` say "phase 0"/"phase 1" about *tensor* phase, and
`crates/fono-mcp-server/src/relevance.rs:9`, `:14`, `:21` say "Stage 1"/"Stage 2"
about the relevance filter's own two-stage design. Both are legitimate technical
prose, not schedule bookkeeping — the widened regex needs to not match them, or they
need rewording.

**Blast radius.** Comments only. No behaviour change. **Binary-size impact:** none.

---

## F-0002 — `[bans] wildcards = "deny"` is dead configuration, and CI never runs `bans` at all

| | |
|---|---|
| **Unit** | 0 (sweep 1.2) |
| **Severity** | S3 — latent risk |
| **Confidence** | high |
| **Location** | `deny.toml:91-97`, `.github/workflows/ci.yml:702-710` |

**Observation.** CI runs `cargo deny check licenses advisories sources` — `bans` is
absent. Running it locally at the anchor commit produces **17 `error[wildcard]`
failures**, every one of them an internal workspace path dependency (e.g.
`crates/fono/Cargo.toml:147` `fono-core = { path = "../fono-core", ... }`, 20 such in
`fono` alone). cargo-deny treats a path dependency with no `version` field as a
wildcard. Under the current config `cargo deny check bans` can never pass, which is
presumably why it was never added to CI.

**Why it is wrong.** The config asserts a policy (`wildcards = "deny"`) that is not
merely unenforced but unenforceable as written, and its presence makes the whole
`[bans]` section look like a live control when nothing in it runs. The genuinely
useful signal in that section — `multiple-versions` — is therefore invisible too
(see `F-0003`).

**Reproduction.** `cargo deny check bans` → `bans FAILED`, 17 wildcard errors, 54
duplicate warnings.

**Suggested direction.** Set `allow-wildcard-paths = true` (cargo-deny's purpose-built
escape for exactly this workspace-path case), then add `bans` to the CI command so the
duplicate-version signal becomes visible. Decide separately whether
`multiple-versions` should stay `warn` or become `deny` with an explicit `skip` list —
`deny` with a curated skip list is the only version that actually prevents drift.

**Blast radius.** CI config and `deny.toml` only. **Binary-size impact:** none directly;
enables the control that governs it.

---

## F-0003 — Two complete D-Bus stacks (zbus 4 and zbus 5) are linked into the Linux binary

| | |
|---|---|
| **Unit** | 0 (sweep 1.2) |
| **Severity** | S3 — latent risk (size) |
| **Confidence** | high |
| **Location** | `crates/fono-hotkey/Cargo.toml` (via `ashpd 0.9.3`), `crates/fono-tray/Cargo.toml` (via `ksni 0.3.4`), `crates/fono-core/Cargo.toml` (via `notify-rust 4.16.0`) |

**Observation.** `cargo deny check bans` reports 54 crates with multiple versions in the
lockfile. Most are cross-target noise (the `windows-*` family, `ndk`, `jni`, `objc2`),
but a subset is genuinely co-linked in the **default Linux `fono` build**, confirmed
via `cargo tree -p fono --target x86_64-unknown-linux-gnu -d`:

- **`zbus` 4.4.0 *and* 5.14.0** — `ashpd 0.9.3` (portal access, via `fono-hotkey`) pins
  zbus 4; `ksni` (tray) and `notify-rust` (notifications, via `fono-core`) pull zbus 5.
  This drags in duplicate `zvariant` 4/5, `zvariant_derive` 4/5, `zvariant_utils` 2/3,
  `zbus_names` 3/4, `zbus_macros` 4/5 — six duplicated crates, i.e. two entire D-Bus
  client implementations.
- `rustix` 0.38.44 + 1.1.4, with `linux-raw-sys` 0.4.15 + 0.9.4 + 0.12.1 (three).
- `winnow` 0.5.40 + 0.7.15 + 1.0.2 (three), `toml_edit` 0.20.2 + 0.25.11,
  `toml_datetime` 0.6.3 + 1.1.1.
- `hashbrown` 0.14.5 + 0.15.5 + 0.17.0, `getrandom` 0.2.17 + 0.4.2,
  `thiserror` 1.0.69 + 2.0.18, `syn` 1.0.109 + 2.0.117, `nom` 7.1.3 + 8.0.0,
  `memmap2` 0.8.0 + 0.9.10, `libloading` 0.7.4 + 0.8.9, `socket2` 0.5.10 + 0.6.3,
  `webpki-roots` 0.26.11 + 1.0.7.

**Why it is wrong.** Binary size is the project's stated top priority, enforced by a
25 MiB gate. Two D-Bus stacks is the single largest avoidable duplication in the graph,
and nothing currently reports it because `bans` never runs (`F-0002`).

**Reproduction.**
`cargo tree -p fono --target x86_64-unknown-linux-gnu -e normal,build -d | grep zbus`.

**Suggested direction.** This is a measurement task before it is a fix: get
`cargo bloat --crates` numbers for the zbus 4 and zbus 5 subtrees to size the prize
(the CI job already produces these reports — see `F-0011`). If it is material, the
lever is `ashpd`, which is the only zbus-4 consumer: check whether a newer `ashpd`
moves to zbus 5, or whether the portal use in `fono-hotkey` is small enough to do
directly over the zbus 5 the binary already links.

**Blast radius.** Dependency graph; no source logic. **Binary-size impact:** this is a
size *reduction* opportunity, magnitude currently unmeasured.

---

## F-0004 — 53 of 140 `unsafe` sites carry no safety contract, and 18 of 19 crates declare no `unsafe` policy

| | |
|---|---|
| **Unit** | 0 (sweep 1.4) |
| **Severity** | S3 — latent risk |
| **Confidence** | high |
| **Location** | see table below |

**Observation.** Counting `unsafe {` blocks, `unsafe fn`/`impl`/`extern` items across
all tracked `.rs`: **140 sites, 87 with a `SAFETY:` note in the 4 preceding lines, 53
without.** Concentration:

| File | Undocumented |
|---|---:|
| `crates/fono-core/src/vk_loader_shim.rs` | 16 |
| `crates/fono-core/src/brain_tap.rs` | 8 |
| `crates/fono-assistant/src/llama_local.rs` | 6 |
| `crates/fono-overlay/src/backends/windows.rs` | 5 |
| `crates/fono-polish/src/llama_local.rs` | 5 |
| `crates/fono-core/src/llama_gen.rs` | 2 |
| `crates/fono-hotkey/src/xerror.rs` | 2 |
| `crates/fono-overlay/src/backends/macos.rs` | 2 |
| 7 more files | 1 each |

Only `crates/fono-http/src/lib.rs:55` declares a crate-root policy
(`#![forbid(unsafe_code)]`). The workspace lint table sets only
`unsafe_op_in_unsafe_fn = "warn"` (`Cargo.toml:270-271`); `undocumented_unsafe_blocks`
is not enabled anywhere.

Two undocumented sites stand out for Stage 2 and are pre-flagged here:
`crates/fono-core/src/llama_gen.rs:258` (`std::mem::transmute_copy` to a raw model
pointer) and `:283` (`std::mem::transmute` of a raw sampler pointer into a safe
wrapper) — transmutes across an FFI boundary with no stated invariant.

**Why it is wrong.** This is the highest-consequence code in the repo and the least
governed. The team demonstrably knows the pattern; it is simply not applied outside one
750-line crate.

**Reproduction.** Script in the session log; re-runnable as a grep over `unsafe` sites
checking the 4 preceding lines for `SAFETY`.

**Suggested direction.** Two separable pieces. (a) Add
`#![deny(unsafe_code)]` at the root of the crates that contain none —
`fono-net-codec`, `fono-download`, `fono-ipc` and any other clean one — so the absence
becomes enforced rather than incidental. (b) For crates that do use it, turn on
`clippy::undocumented_unsafe_blocks` and work the 53 down; several are trivially
documentable (the `dlopen`/`dlsym` wrappers in `vk_loader_shim.rs` share one
contract). Note (b) makes clippy fail until the backlog clears, so it wants to land
behind the documentation, not before.

**Blast radius.** No behaviour change; comments plus lint attributes.
**Binary-size impact:** none.

---

## F-0005 — The `ttf-parser` advisory-ignore justification names a dependency path that does not exist

| | |
|---|---|
| **Unit** | 0 (sweep 1.3) |
| **Severity** | S3 — latent risk |
| **Confidence** | high |
| **Location** | `deny.toml:35-39` |

**Observation.** The comment says `ttf-parser` "is pulled in transitively by the
overlay font-rendering stack (cosmic-text/swash)". **Neither `cosmic-text` nor `swash`
is in `Cargo.lock` at all.** The actual path is
`fono-overlay → ab_glyph 0.2.32 → owned_ttf_parser 0.25.1 → ttf-parser 0.25.1`, and it
is present on all three target graphs (`x86_64-unknown-linux-gnu`,
`x86_64-pc-windows-msvc`, `aarch64-apple-darwin`) — i.e. **compiled into every shipped
binary**.

**Why it is wrong.** The ignore's stated exit condition ("revisit if the font stack
migrates off it") refers to a migration that has already happened. More materially, the
comment reads as though this were incidental lockfile noise like the gtk3 block above
it; it is not. An unmaintained font parser is linked into the shipped binary and parses
font files — a real, if low-likelihood, untrusted-input surface that the current
rationale obscures.

**Reproduction.** `cargo tree -p fono -i ab_glyph --target all -e normal` and
a `Cargo.lock` scan for `cosmic-text` / `swash` (both absent).

**Suggested direction.** Rewrite the rationale to state the real path and the real
exposure, and record the actual exit condition (an `ab_glyph`/`owned_ttf_parser`
release that moves off `ttf-parser`, or a maintained fork). Separately, note for Unit M
that font-file parsing is an input surface worth a look — the overlay loads system
fonts.

**Blast radius.** Documentation. **Binary-size impact:** none.

---

## F-0006 — One advisory ignore is stale and four license allowances are unmatched

| | |
|---|---|
| **Unit** | 0 (sweep 1.3) |
| **Severity** | S5 — improvement |
| **Confidence** | high |
| **Location** | `deny.toml:31`, `deny.toml:64-66`, `deny.toml:74` |

**Observation.** `cargo deny check advisories licenses` passes but emits:
`advisory-not-detected` for `RUSTSEC-2024-0429` (glib) — no crate in the graph matches
it any more; and `license-not-encountered` for `LGPL-3.0`, `LGPL-3.0-only`,
`LGPL-3.0-or-later` and `Unicode-DFS-2016`.

**Why it is suboptimal.** Stale entries make the ignore list look larger and more
alarming than the real exposure, and the whole point of the meticulous per-entry
rationale is that a reader can trust the list is current. The gtk3 block's other ten
entries were checked and **do still hold** — `gtk`, `glib`, `atk`, `atk-sys`, `gdk`,
`gdk-sys`, `gdk-pixbuf`, `gdk-pixbuf-sys`, `gtk-sys`, `gtk3-macros` and
`proc-macro-error` are all present in `Cargo.lock` and absent from all three real
target graphs, exactly as documented. The `quick-xml` entries also hold: present on
Linux only, via `wayland-scanner 0.31.10` (a build-time proc-macro), exactly as
documented.

**Reproduction.** `cargo deny check advisories licenses` warning output.

**Suggested direction.** Drop `RUSTSEC-2024-0429` and the four unmatched license
strings. Keep the rest of the block unchanged.

**Blast radius.** `deny.toml` only. **Binary-size impact:** none.

---

## F-0007 — One advisory ignore does not say what it is for

| | |
|---|---|
| **Unit** | 0 (sweep 1.3) |
| **Severity** | S5 — improvement |
| **Confidence** | high |
| **Location** | `deny.toml:34` |

**Observation.** `"RUSTSEC-2025-0141", # transitive unmaintained crate` — every other
entry in the file names the crate and states an exit condition; this one names neither.
It also sits inside the gtk3 comment block, so a reader will assume it is part of that
justification, which is unverifiable as written.

**Suggested direction.** Name the crate and the path, or drop the entry if it no longer
matches (it is not reported as `advisory-not-detected`, so it currently does match
something).

**Blast radius.** `deny.toml` only. **Binary-size impact:** none.

---

## F-0008 — The unpinned-asset sentinel silently disables integrity checking, and three copies of the predicate disagree

| | |
|---|---|
| **Unit** | 0 (spotted during sweep 1.7) |
| **Severity** | S3 — latent risk, **pre-flagged for Stage-2 verification as a possible S2** |
| **Confidence** | medium — the download-layer bypass is certain; whether a user can reach it is not yet traced |
| **Location** | `crates/fono-download/src/lib.rs:47`, `crates/fono-audio/src/wake_registry.rs:190`, `:369`, `:475`, `crates/fono-audio/src/speaker.rs:658`, `crates/fono-tts/src/supertonic/mod.rs:272` |

**Observation.** An all-zeros SHA-256 is an "unpinned" sentinel, and
`crates/fono-download/src/lib.rs:47` responds by **accepting the downloaded file
without verifying it** (`:56` logs `sha256=… (unpinned)` and returns Ok). The default
wake-word model — `hey_fono`, `WakeModelClass::Default`, first in the table — carries
that sentinel at `crates/fono-audio/src/wake_registry.rs:190`.

Three independent copies of the "is it pinned" predicate exist and do not agree:

| Location | Predicate |
|---|---|
| `crates/fono-audio/src/speaker.rs:658` | `len() == 64 && !all-zeros` |
| `crates/fono-audio/src/wake_registry.rs:369` | `!all-zeros` only |
| `crates/fono-download/src/lib.rs:47` | `!all-zeros` only |

Under the two loose copies, the empty string and `"0"`-free garbage of any length are
both "pinned", and any all-zeros string of any length disables verification.

A test at `crates/fono-audio/src/wake_registry.rs:461-476` **asserts the default model
stays unpinned** ("hey_fono is expected to stay UNPINNED until its Phase B artifact
ships"). That is an S4 in its own right: it locks the disarmed state in place and will
fail the day someone arms it.

**Why it is wrong.** A download path that can be silently unverified is a supply-chain
hole, and it sits on the default wake model rather than an obscure opt-in. There is a
mitigating guard — `crates/fono-audio/src/wake_registry.rs:369` and `:521` appear to
gate *fetchability* on pinned-ness, which would make `hey_fono` unfetchable rather than
unverified — but that has not been traced end to end, and it means the shipped default
wake model may simply not work. Either way there is a defect; Stage 2 determines which.

**Reproduction.** Not yet reproduced — pre-flagged for Unit E (audio) and Unit N
(download). The specific question to answer: can any user-reachable path call
`fono_download::download` with an all-zeros expected hash?

**Suggested direction.** Regardless of the reachability answer: make the sentinel
non-bypassable at the download layer — an unpinned asset should be a hard error there,
with "unpinned is allowed" an explicit opt-in argument rather than an inferred property
of the hash string. Collapse the three predicates into one shared helper. Replace the
"must stay unpinned" assertion with one that fails if the *default* model is unpinned.

**Blast radius.** Download path and the wake/speaker/TTS asset tables.
**Binary-size impact:** none.

---

## F-0009 — Editing a bench baseline cannot re-run the gate that consumes it

| | |
|---|---|
| **Unit** | 0 (sweep 1.10) |
| **Severity** | S3 — latent risk |
| **Confidence** | high |
| **Location** | `.github/workflows/ci.yml:7-21`, `docs/bench/baseline-comfortable-tiny-en.json`, `docs/bench/baseline-cloud-groq.json` |

**Observation.** Both `push` and `pull_request` carry `paths-ignore: ["**/*.md",
"docs/**", "plans/**", …]`. Both equivalence baselines live under `docs/bench/`. A PR
that changes only a baseline therefore runs **no CI at all** — including the
equivalence gate whose verdicts that file defines.

**Why it is wrong.** `docs/bench/README.md:112-134` sets out careful governance for
baseline updates ("baseline bumps must be their own reviewable commit"). A baseline
bump in its own commit is precisely the case that gets zero verification. The control
and the filter are in direct conflict.

**Reproduction.** Inspection; would be confirmed by pushing a baseline-only branch.

**Suggested direction.** Add `!docs/bench/**` as a negated pattern to both
`paths-ignore` lists, or move the baselines out of `docs/` into `tests/`. The second is
tidier — the baselines are test fixtures that happen to be published, not documentation.

**Blast radius.** CI config, or a file move plus its two reader paths.
**Binary-size impact:** none.

---

## F-0010 — The criterion benchmark is compiled on every run and executed on none

| | |
|---|---|
| **Unit** | 0 (sweep 1.10) |
| **Severity** | S5 — improvement |
| **Confidence** | high |
| **Location** | `crates/fono-bench/benches/orchestrator.rs`, `crates/fono-bench/Cargo.toml:90-92`, `.github/workflows/ci.yml:99` |

**Observation.** No workflow and no script runs `cargo bench`. The only reference in CI
is a comment at `.github/workflows/ci.yml:108`. `cargo clippy --all-targets` compiles
the bench, so the project pays the build cost of a network-free, deterministic
orchestrator benchmark and discards the numbers. The only standing perf signal is the
p95 assertion in `crates/fono-bench/tests/latency_smoke.rs:28`, which CI does run.

**Suggested direction.** Either run it and record a trend (cheapest useful version: run
it on `main` pushes only and archive the criterion output as an artifact), or convert
its measurement into a threshold assertion alongside the existing latency smoke test
and delete the criterion harness. Doing neither is the current state and is the only
option with no upside.

**Blast radius.** CI config. **Binary-size impact:** none.

---

## F-0011 — `cargo-bloat` reports are generated every CI run with no consumer

| | |
|---|---|
| **Unit** | 0 (sweep 1.10) |
| **Severity** | S5 — improvement |
| **Confidence** | medium — the artifacts demonstrably exist; whether a human reads them is inferred |
| **Location** | `.github/workflows/ci.yml:388-425` |

**Observation.** The `size-budget` job runs `cargo bloat --crates -n 30` and `-n 50`
and uploads both as artifacts with `if: always()`, on three matrix rows. Nothing
consumes them: no diff against a previous run, no threshold, no comment on the PR.

**Why it is suboptimal.** This is a paid-for, per-commit trend signal on the project's
top-priority metric, currently going to waste. It is also exactly the data `F-0003`
needs to size the zbus duplication.

**Suggested direction.** The cheap version is to diff the current run's `--crates`
output against the same artifact from the merge-base and post the top movers. No new
dependency required — the reports are already JSON-able via `cargo bloat --message-format`.

**Blast radius.** CI config. **Binary-size impact:** none directly.

---

## F-0012 — Six feature flags are never compiled, linted or tested by any gate

| | |
|---|---|
| **Unit** | 0 (sweep 1.6) |
| **Severity** | S3 — latent risk |
| **Confidence** | high |
| **Location** | `crates/fono/Cargo.toml` feature table; `.github/workflows/ci.yml:99`, `:102`, `:340`, `:697` |

**Observation.** Every gate builds default features (or, on Windows,
`--no-default-features --features windows-defaults`). Mapping the 18 `fono` features
against what any gate compiles:

| Feature | Compiled anywhere? |
|---|---|
| `enigo` → `fono-inject/enigo-backend` | **never** |
| `bench-actions` | **never** |
| `fono-audio/cpal-backend` | **never** (and `tests/check.sh:84-96` actively asserts its *absence*) |
| `accel-cuda`, `accel-rocm`, `accel-openblas` | **never** |
| `accel-coreml` | never (macOS job builds `accel-metal` only) |
| `accel-vulkan` | built by the `size-budget` gpu row — **but never clippy'd and never tested** |
| `accel-metal` | built by `size-budget-macos` — same caveat |

Additionally `cargo deny` runs with `[graph] all-features = false` (`deny.toml:5`), so
the licence and advisory checks also see only the default graph — a provider enabled by
a non-default feature could carry an unreviewed licence.

**Why it is wrong.** `cpal-backend` is the documented fallback capture backend for
hosts without PipeWire, and `enigo` is a documented injection backend; both are
user-reachable build options that no gate has compiled. Code that is never compiled
rots silently, and the first person to find out is a user building from source with a
non-default flag.

**Reproduction.** `cargo check -p fono --features enigo,bench-actions` and
`cargo check -p fono-audio --features cpal-backend` — not run here, because that
belongs in the fix, not the audit.

**Suggested direction.** Add one cheap CI job that runs `cargo check` (not build, not
test) over a small set of otherwise-uncompiled combinations: `enigo`, `bench-actions`,
`fono-audio/cpal-backend`. `cargo check` is fast and catches the rot that matters.
The `accel-*` rows genuinely need toolchains CI does not have — record that as accepted
risk rather than pretending otherwise. Separately consider whether `cargo deny` should
run a second pass with `all-features = true`.

**Blast radius.** CI config. **Binary-size impact:** none.

---

## F-0013 — The ignored-test tier is coherent, with one exception worth revisiting

| | |
|---|---|
| **Unit** | 0 (sweep 1.8) |
| **Severity** | S5 — improvement |
| **Confidence** | high |
| **Location** | 24 sites; see classification |

**Observation.** All 24 `#[ignore]` tests carry a reason string, which is better than
most codebases manage. Classification:

| Precondition | Count | Sites |
|---|---:|---|
| `FONO_TEST_ASSISTANT_GGUF` | 7 | `crates/fono-assistant/src/llama_local.rs:3537`, `:3650`, `:3985`, `:4050`, `:4115`, `:4198`, `:4255` |
| `FONO_TEST_VOCAB_GGUF` | 5 | `crates/fono-core/src/llama_gen.rs:491`, `:565`, `:578`, `:604`, `:685` |
| Converted `.ort` model + linked runtime | 4 | `crates/fono-tts/src/piper.rs:392`, `:423`, `crates/fono-tts/src/kokoro.rs:433`, `crates/fono-tts/src/supertonic/engine.rs:610` |
| Live network service | 4 | `crates/fono-assistant/src/mcp_client.rs:627`, `crates/fono/src/daemon.rs:6210`, `crates/fono/src/actions/mod.rs:4076`, `:4153` |
| Host capability | 2 | `crates/fono-core/src/vulkan_probe.rs:684`, `crates/fono-inject/tests/clipboard_smoke.rs:6` |
| Perf (CI **does** run these) | 2 | `crates/fono-bench/tests/latency_smoke.rs:28`, `:84` |

**The exception.** CI already downloads a SHA-pinned whisper `tiny.en` for the
equivalence gate (`.github/workflows/ci.yml:132-161`). The five `FONO_TEST_VOCAB_GGUF`
tests in `crates/fono-core/src/llama_gen.rs` need only a *vocabulary*, and they cover
the tool-calling grammar rails — logic where a silent regression produces wrong
behaviour rather than a crash. That precondition is now satisfiable in CI at roughly
the cost already being paid.

**Suggested direction.** Extend the existing model-download step to fetch one small
pinned GGUF and set `FONO_TEST_VOCAB_GGUF`, then run those five with `--ignored`
alongside the latency pair. Leave the other 17 as they are; their preconditions are
genuinely out of CI's reach.

**Blast radius.** CI config plus one download. **Binary-size impact:** none.

---

## F-0014 — Lint-escape inventory: the blanket file-level allows are the ones to question

| | |
|---|---|
| **Unit** | 0 (sweep 1.5) |
| **Severity** | S5 — improvement |
| **Confidence** | high |
| **Location** | 12 file-level sites; ~230 inline sites |

**Observation.** Twelve `#![allow(...)]` at file scope:
`crates/fono-overlay/src/renderer.rs:16`, `crates/fono-overlay/src/r3d.rs:37`,
`crates/fono-overlay/src/cortex.rs:84` (multi-line blocks — the widest escapes in the
repo), the four overlay backends (`macos.rs:36`, `wayland_layer_shell.rs:25`,
`windows.rs:40`, `winit_x11.rs:13`), the three `significant_drop_tightening` sites
(`crates/fono-assistant/src/llama_local.rs:8`, `crates/fono-polish/src/llama_local.rs:20`,
`crates/fono-stt/src/whisper_local.rs:10`), and two overlay examples.

Inline `#[allow]` distribution is dominated by size/complexity lints — `too_many_lines`
(70), `too_many_arguments` (25), `cognitive_complexity` (18),
`significant_drop_tightening` (13), the cast family (~26), `dead_code` (8). Nothing
uses `#[expect(...)]`, which would make a stale escape fail the build rather than sit
silently.

**Why it matters, and what it does not mean.** 70 `too_many_lines` escapes is a fact
about the codebase (three files over 4,000 lines), not a defect list — suppressing that
lint per-function is a reasonable response. The ones that carry real risk are the three
multi-line blanket blocks in the overlay, which switch off several lints for
2,000–2,700-line files at once and are therefore capable of hiding a genuine defect;
they are already scheduled for scrutiny under Unit M.
`significant_drop_tightening` at file scope on the three local-inference files is the
second cluster worth attention — it is a lock-lifetime lint, suppressed wholesale in
exactly the three files where lock lifetime is load-bearing (Unit I).

**Suggested direction.** Prefer `#[expect(...)]` over `#[allow(...)]` for new escapes
so they self-expire. Narrow the three overlay blanket blocks to the specific functions
that need them. Neither is urgent; both are cheap.

**Blast radius.** Lint attributes only. **Binary-size impact:** none.

---

## F-0015 — Dead-code escapes are mostly justified; two are not

| | |
|---|---|
| **Unit** | 0 (sweep 1.7) |
| **Severity** | S5 — improvement |
| **Confidence** | medium — a full unreachable-item analysis needs compiler output, see note |
| **Location** | 12 sites |

**Observation.** Of the 12 `dead_code` escapes, nine are legitimately
platform- or feature-conditional and say so:
`crates/fono-audio/src/playback.rs:90`, `crates/fono/src/lib.rs:107`,
`crates/fono-core/src/locale.rs:486`, `crates/fono-assistant/src/sse.rs:18-20`,
`crates/fono-stt/src/deepgram_streaming.rs:136`,
`crates/fono-stt/src/speechmatics.rs:71`, `:74`,
`crates/fono-overlay/src/backends/wayland_layer_shell.rs:150`,
`crates/fono-mcp-server/src/server.rs:253`.

Two are not conditional — they are unfinished work parked in the tree:

- `crates/fono/src/live.rs:597` — `#[allow(dead_code)] // stub: wired in Slice B once
  translator gets preview-text feedback.` A stub with no caller and a schedule comment
  (also an `F-0001` violation).
- `crates/fono-stt/src/speechmatics.rs:74` — "captured so `[stt.prompts]` doesn't error;
  not yet sent on the wire". This one is worse than dead code: it means a user can set
  a config key, get no error, and have it silently ignored. Pre-flagged for Unit J
  (config) and Unit K (provider divergence) — the question is how many other config
  keys are accepted and dropped.

There are also four `TODO`s in `crates/fono-core/src/provider_catalog.rs:399`, `:466`,
`:533`, `:541`, two of which ("verify against Anthropic's current model list") describe
data that goes stale on someone else's schedule — a model catalog that drifts out of
date produces user-visible wrong behaviour, not a compile error.

**Note on completeness.** A true unreachable-item analysis needs `cargo check`
warnings across the feature matrix, which `F-0012` shows is not currently obtainable
from any single build. This sweep is therefore a floor, not a ceiling.

**Suggested direction.** Delete the `live.rs` stub (it can come back with the work that
needs it). Make silently-ignored config keys warn at load time — that is a Unit J
recommendation and should be answered once for all providers, not per key.

**Blast radius.** Small. **Binary-size impact:** none.

---

## Panic-surface map (sweep 1.9 — no finding, input to Stage 2)

136 panic-capable sites in non-test source: 112 `.expect(`, 21 `.unwrap()`,
2 `unreachable!`, 1 `panic!`. No `todo!`/`unimplemented!`. For a 156k-line codebase
that is a low density and suggests the `?`-propagation discipline is generally good;
this is a map for the per-unit reading, not a defect list. Concentration:

| Count | File | Owning unit |
|---:|---|---|
| 11 | `crates/fono-core/src/brain_tap.rs` | H |
| 10 | `crates/fono/src/session.rs` | A |
| 9 | `crates/fono-audio/src/playback.rs` | E |
| 7 | `crates/fono-net/src/llm_server/mod.rs` | G |
| 7 | `crates/fono/src/speak_stream.rs` | C |
| 6 | `crates/fono-core/src/turn_trace.rs` | J |
| 6 | `crates/fono/src/wizard.rs` | N |
| 5 | `crates/fono-net/src/discovery/mod.rs` | G |
| 5 | `crates/fono-net/src/web_settings/mod.rs` | G |

`crates/fono-audio/src/playback.rs` at 9 is the one to read first — it is the only
entry on a real-time path, where a panic kills audio output rather than returning an
error.

**Note.** SPDX headers were also checked during this sweep: **all 272 tracked `.rs`
files carry the correct header.** The rule is enforced by review only, and review has
held perfectly. Adding the four-line gate (the pattern already exists at
`tests/check.sh:107`) would make that permanent at near-zero cost — recorded here
rather than as its own finding, because there is no defect, only an unguarded invariant.

---

## F-0016 — Coverage baseline: 56.9% overall, but the daemon event loop is at 7%

| | |
|---|---|
| **Unit** | 0 (sweep 1.1) |
| **Severity** | S3 — latent risk |
| **Confidence** | high |
| **Location** | whole workspace; hot spots listed below |

**Method.** `cargo llvm-cov --workspace --tests --lib` at the anchor commit, default
features, `x86_64-unknown-linux-gnu`, system LLVM 22.1.8. This is the first coverage
measurement the project has ever had.

**Caveats, stated up front.** The number covers the default feature set on Linux only,
so everything in `F-0012` (never-compiled features) is invisible here, as is all
macOS/Windows backend code — those files report 0% because they were not built, not
because they are untested. `--tests --lib` matches the local gate, not CI's
`--all-targets`. And coverage measures *execution*, not assertion quality: a line
executed by a test that asserts nothing still counts as covered. Treat this as a map of
where to look, not a score.

**Headline.** 125,382 regions / 72,640 lines instrumented. **Line coverage 55.0%,
region coverage 56.9%, function coverage 62.0%.** For a codebase with tests in 201 of
272 files, that is a respectable floor — but it is very unevenly distributed.

### Per-crate line coverage

| Crate | Lines | Uncovered | Covered |
|---|---:|---:|---:|
| `fono-net-codec` | 558 | 5 | **99.1%** |
| `fono-core` | 11,359 | 1,725 | 84.8% |
| `fono-net` | 2,936 | 541 | 81.6% |
| `fono-audio` | 3,248 | 813 | 75.0% |
| `fono-tts` | 4,967 | 1,343 | 73.0% |
| `fono-stt` | 5,020 | 1,728 | 65.6% |
| `fono-mcp-server` | 2,392 | 867 | 63.8% |
| `fono-http` | 268 | 104 | 61.2% |
| `fono-polish` | 1,828 | 815 | 55.4% |
| `fono-tray` | 908 | 427 | 53.0% |
| `fono-assistant` | 5,821 | 2,823 | 51.5% |
| `fono-bench` | 3,966 | 2,064 | 48.0% |
| `fono-overlay` | 4,394 | 2,349 | 46.5% |
| `fono-download` | 98 | 55 | 43.9% |
| `fono-hotkey` | 1,151 | 673 | 41.5% |
| `fono-update` | 451 | 267 | 40.8% |
| **`fono`** | **21,930** | **15,051** | **31.4%** |
| `fono-inject` | 1,210 | 918 | 24.1% |
| `fono-ipc` | 135 | 105 | 22.2% |

### The ten biggest holes by uncovered lines

| Uncovered | Coverage | File | Unit |
|---:|---:|---|---|
| 3,989 | **7.1%** | `crates/fono/src/daemon.rs` | **B** |
| 2,764 | 27.7% | `crates/fono/src/session.rs` | **A** |
| 2,074 | 24.0% | `crates/fono-assistant/src/llama_local.rs` | **I** |
| 1,709 | **6.0%** | `crates/fono/src/cli.rs` | N |
| 1,699 | 22.1% | `crates/fono/src/assistant.rs` | **C** |
| 1,316 | 30.8% | `crates/fono-overlay/src/renderer.rs` | M |
| 1,151 | **0.0%** | `crates/fono-bench/src/bin/fono-bench.rs` | N |
| 1,010 | 30.1% | `crates/fono/src/install/linux.rs` | N |
| 1,009 | **10.2%** | `crates/fono/src/doctor.rs` | N |
| 966 | 41.9% | `crates/fono/src/wizard.rs` | N |

**What this actually tells us.**

1. **`daemon.rs` at 7.1% is the finding.** The daemon event loop is the component that
   fans hotkey events to the session orchestrator, tray, IPC server and overlay — 33
   spawn sites, all the re-entrancy risk in the product — and 3,989 of its 4,294 lines
   have never been executed by a test. It is simultaneously the second-riskiest unit and
   the least exercised. Everything downstream of it (`session.rs` 27.7%,
   `assistant.rs` 22.1%) is in the same condition.

2. **`fono-ipc` at 22.2% versus `fono-net-codec` at 99.1%** is a sharp sibling
   divergence inside a single review unit (F, untrusted-input decoders). Both parse
   length-prefixed frames off a socket; one is exhaustively tested and one is barely
   touched. `fono-ipc` is only 135 lines, so this is a cheap gap to close and a strong
   argument that Unit F should look hardest at the IPC side.

3. **`fono-inject` at 24.1%, with `inject.rs` and `focus.rs` both at 0.0%.** Some of
   this is unavoidable — these need a live display server — but "needs a display" and
   "no test at all" are different states, and the crate has exactly one integration test
   (`crates/fono-inject/tests/clipboard_smoke.rs`), itself `#[ignore]`d.

4. **The `fono` crate's 31.4% is dominated by genuinely hard-to-test surface** —
   `cli.rs` (argument parsing, 6.0%), `doctor.rs` (10.2%), `install/linux.rs` (30.1%),
   `wizard.rs` (41.9%). This is the first-run and diagnostics blast radius, and it is
   also where 6 of the 136 panic sites live. Low coverage here is normal for this kind
   of code; it is worth flagging that `wizard.rs` in particular has the highest
   consequence per defect of anything in the list, because a new user meets it before
   anything else works.

5. **The strong crates are strong for a reason.** `fono-net-codec` at 99.1% and
   `fono-core` at 84.8% show the project can and does test thoroughly where the code is
   pure and the interfaces are narrow. The coverage gradient tracks statefulness almost
   perfectly, which is the expected shape — and it means the low numbers are a
   *testability* problem, not a discipline problem.

**Suggested direction.** Not "raise coverage" — that is a metric, not a goal. Two
concrete items: (a) close the `fono-ipc` gap, because it is 135 lines of frame decoding
with adversarial input and no excuse; (b) treat `daemon.rs`'s 7% as an input to the
Unit B reading rather than a task — the reading will establish whether the code is
untestable (in which case the finding is structural) or merely untested. Consider
recording this baseline so future movement is visible; `cargo llvm-cov` needs no
repo changes and no new dependency.

**Blast radius.** None — measurement only. **Binary-size impact:** none.

---

# Stage 1 — Reprioritised Stage 2 order (Task 1.11)

The sweeps move three units and confirm the rest. Revised order, with the reason for
each change:

| New | Old | Unit | Why it moved |
|---:|---:|---|---|
| 1 | 2 | **B — Daemon event loop** | **Promoted.** 7.1% coverage on the highest-fan-out component in the product. Lowest-evidence, highest-consequence code in the repo. |
| 2 | 1 | A — Session orchestration | Unchanged in substance; 27.7% coverage confirms the original ranking. |
| 3 | 3 | C — Assistant runtime | 22.1% coverage confirms. |
| 4 | 6 | **F — Untrusted input decoders** | **Promoted.** `fono-ipc` at 22.2% against `fono-net-codec` at 99.1% — a concrete, cheap, adversarial-input gap rather than a theoretical one. |
| 5 | 5 | E — Audio real-time path | Holds. Now also carries `F-0008` (unpinned wake model) and the 9 panic sites in `playback.rs`. |
| 6 | 4 | D — State machines | Slipped one place; nothing new against it, the promotions moved past it. |
| 7 | 9 | **I — Local inference backends** | **Promoted.** `llama_local.rs` is the third-largest hole (2,074 uncovered), and `F-0014` shows lock-lifetime linting is switched off wholesale in exactly these three files. |
| 8 | 7 | G — Network servers | Holds. `fono-net` at 81.6% is comparatively well covered. |
| 9 | 8 | H — FFI and `unsafe` | Holds, now anchored by `F-0004`'s 53 undocumented sites and the two `llama_gen.rs` transmutes. |
| 10 | 10 | J — Config, paths, persistence | Holds. Picks up the silently-ignored-config-key question from `F-0015`. |
| 11 | 11 | K — Cloud provider backends | Holds. |
| 12 | 14 | **N — Update, download, CLI, wizard** | **Promoted.** `doctor.rs` 10.2%, `cli.rs` 6.0%, `wizard.rs` 41.9%, `fono-update` 40.8%, `fono-download` 43.9% — and `F-0008`'s download-layer question lands here. |
| 13 | 12 | L — Injection and platform | Slipped. 24.1% coverage is bad, but most of it needs a live display server, so reading buys less than elsewhere. |
| 14 | 13 | M — Rendering and overlay | Slipped to last. Largest remaining blanket lint escapes, but defects here are visible rather than silent. |

**Cross-cutting items that Stage 2 must carry into every unit** (from the sweeps, to
avoid re-deriving them per unit): the `std`/`tokio` sync-primitive mix; the
silently-accepted-config-key pattern (`F-0015`); the duplicated-predicate pattern
(`F-0008`); and sibling divergence between near-identical implementations, which showed
up independently in the decoders, the three `llama_local.rs` files, and the provider
backends.

---

# Stage 1 — Summary

**16 findings.** None at S1 or S2 — no confirmed correctness-critical defect surfaced
from mechanical sweeps, which is the expected and healthy result for this stage.
One S3 (`F-0008`) is pre-flagged as a possible S2 pending Stage-2 reachability tracing.

| Severity | Count | IDs |
|---|---:|---|
| S1 | 0 | — |
| S2 | 0 | — |
| S3 | 8 | `F-0002`, `F-0003`, `F-0004`, `F-0005`, `F-0008`, `F-0009`, `F-0012`, `F-0016` |
| S4 | 1 | `F-0001` |
| S5 | 7 | `F-0006`, `F-0007`, `F-0010`, `F-0011`, `F-0013`, `F-0014`, `F-0015` |

**What the sweeps say about the codebase overall.** The controls that exist are
unusually good — the size budget, the linkage allowlists, the pinned-audio equivalence
gate, the per-entry advisory rationales, the reason string on every `#[ignore]`. Ten of
the fourteen advisory ignores were verified still accurate. SPDX compliance is perfect
across all 272 files. The panic density is low. Where the code is pure, it is tested to
99%.

The weaknesses cluster in one place: **controls that were configured but never wired
up.** `bans` is configured and not run. The comment-hygiene regex enforces a narrower
rule than the one written down. The criterion bench is built and not executed. The
bloat reports are generated and not read. Six feature flags are shippable and never
compiled. Baselines live where CI cannot see them change. Individually each is small;
together they are the dominant pattern in Stage 1, and every one of them is cheap to
close.



---

# Stage 2 — Unit B: daemon event loop

**Scope:** `crates/fono/src/daemon.rs` — process lifecycle (`run()`, lines 1–1529),
the hotkey-event consumer task, the tray-action dispatcher task, and the IPC accept
loop. Excludes `handle_client` request handling and the `*_via_tray` helpers, which
are Unit C.

**Why first:** `F-0016` measured this file at **7.1% line coverage** (3,989 uncovered
lines) — the least-executed and highest-fan-out code in the product.

**Coverage of lenses:** all eight applied. Lenses 2 (error paths) and 5 (boundary
values) produced no findings in this scope — every fallible call in `run()` either
propagates with `.context(...)` or logs and degrades, and the loop takes no
user-supplied scalar input. The findings cluster in lenses 1, 3, 7 and 8.

## F-B001 — SIGTERM is unhandled, so the shipped systemd units never shut down cleanly

**Severity:** S2 · **Confidence:** high

**Location:** `crates/fono/src/daemon.rs:1493-1528`, `packaging/systemd/fono.service:7-9`

**Observation.** The main loop selects on exactly one shutdown source:

```
_ = tokio::signal::ctrl_c() => { info!("ctrl-c received; shutting down"); break; }
```

`ctrl_c()` is SIGINT only. A workspace-wide search for `SIGTERM`, `unix::signal` or
`SignalKind` returns nothing. The project ships three systemd units
(`packaging/systemd/fono.service`, `packaging/slackbuild/fono/fono.service`,
`assets/fono.service`), all `Type=simple`, so `systemctl --user stop fono`, a logout,
and a shutdown all deliver SIGTERM. With no handler installed, the default disposition
terminates the process immediately.

**Why it is wrong.** The clean path at `daemon.rs:1527-1528` — unlink the socket, return
`Ok(())`, unwind `run()`'s locals — is the *only* path that runs the destructors the
code depends on for network hygiene. `AdvertiserHandle::drop`
(`crates/fono-net/src/discovery/advertiser.rs:93-105`) is what sends the mDNS goodbye,
and `wyoming_ctl` / `llm_ctl` / `discovery` are `run()` locals. On SIGTERM none of it
runs: LAN peers keep a dead Fono in their service cache until the record TTL expires,
and any in-flight transcription is dropped without the text ever reaching the user or
the history DB.

The path that *is* clean — Ctrl-C — is the developer path, reachable only when the
daemon runs in a foreground terminal. Both paths real users take (see also `F-B002`
for the tray) skip the cleanup.

**Reproduction.** `systemctl --user start fono` with `[server.wyoming].enabled = true`,
then browse `_wyoming._tcp` from a second host (`avahi-browse -r _wyoming._tcp`).
`systemctl --user stop fono`; the service stays in the browser's cache. Repeat with the
daemon in a terminal and Ctrl-C: the goodbye arrives and the entry vanishes at once.

**Suggested direction.** Add a SIGTERM arm to the same `select!` that breaks the loop
identically, so both signals converge on the existing clean path:

```rust
#[cfg(unix)]
let mut sigterm = tokio::signal::unix::signal(
    tokio::signal::unix::SignalKind::terminate())?;
```

`tokio::signal::unix` is already in the dependency graph (the `signal` feature is on —
`ctrl_c` needs it), so this is **net-zero on binary size**. Worth pairing with a bounded
drain of in-flight work before the break; see `F-B002`.

**Blast radius.** One `select!` arm plus one binding, both `#[cfg(unix)]`. No API change.

## F-B002 — Tray "Quit" calls `process::exit`, skipping the cleanup two comments promise

**Severity:** S2 · **Confidence:** high

**Location:** `crates/fono/src/daemon.rs:1184-1187`; contradicted comments at
`crates/fono/src/daemon.rs:332-334` and `crates/fono/src/daemon.rs:346-348`

**Observation.** The tray Quit handler is:

```rust
TrayAction::Quit => {
    let _ = std::fs::remove_file(paths.ipc_socket());
    std::process::exit(0);
}
```

`std::process::exit` does not unwind and does not run destructors. Two comments in the
same file state the opposite as a design guarantee — of `wyoming_ctl`: *"Held for the
daemon's lifetime; dropped on exit, which closes the listener and fires the mDNS
goodbye"* (`daemon.rs:332-334`), and of `llm_ctl`: *"dropping the handles closes the
listener and fires the mDNS goodbye"* (`daemon.rs:346-348`).

**Why it is wrong.** Those two sentences are true only on the Ctrl-C path. Quit is the
ordinary way a desktop user stops a tray application, and it is the one path where the
author explicitly wrote out the socket cleanup by hand — which shows the intent was a
tidy exit, but the hand-written version covers only the socket and misses the mDNS
goodbye, the listener close, and any in-flight dictation. Taken with `F-B001`, **all
three** non-developer exits (tray Quit, `systemctl stop`, logout) skip the documented
teardown.

This is also why the comments matter beyond style: a future reader adding a resource
that needs flushing on exit will read `daemon.rs:332-334`, conclude that dropping is
sufficient, and be wrong for the majority of real exits.

**Reproduction.** As `F-B001`, but quit from the tray menu instead of `systemctl stop`.

**Suggested direction.** Make Quit converge on the same clean path as the other two
rather than duplicating a partial version of it: give the accept loop a shutdown
channel, have `TrayAction::Quit` send on it, and let `run()` return normally. That
retires the hand-written `remove_file`, makes the two comments true, and gives all
three exits one implementation to maintain. Keep `process::exit` only as a
watchdog-timeout fallback if a drain is added and could hang.

**Blast radius.** One `mpsc::channel(1)`, one extra `select!` arm, one changed tray arm.
Contained within `run()`.

## F-B003 — Every `Stop*` hotkey arm spawns while every `Start*` awaits, which is the race the file documents guarding against

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono/src/daemon.rs:881-1042`; specifically `883` vs `893`

**Observation.** In the hotkey-event consumer, the arms split cleanly by name:

| Arm | Line | Handling |
|---|---:|---|
| `StartRecording` | 883 | awaited inline |
| `StopRecording` | 893 | **`tokio::spawn`** |
| `Cancel` | 900 | **`tokio::spawn`** |
| `StartLiveDictation` | 916 | awaited inline |
| `StopLiveDictation` | 937 | **`tokio::spawn`** |
| `StartAssistant` | 966 | awaited inline |
| `StopAssistant` | 974 | **`tokio::spawn`** |
| `StopAssistantPlayback` | 980 | **`tokio::spawn`** |
| `RestartAssistant` | 1003 | awaited inline |
| `EnterAssistantLive` | 1019 | awaited inline |
| `ExitAssistantLive` | 1030 | awaited inline |

Events arrive on one channel and are processed in order, but a spawned arm returns to
the `recv()` immediately — so the *next* event begins executing while the previous
handler is still running.

**Why it is wrong.** The file already argues, twice, that this is unsafe. At
`daemon.rs:1014-1018`, on `EnterAssistantLive`: *"Awaited inline — like `StartAssistant`
— so the persistent session handle is recorded before the next event (e.g. the
exit-tap) is dequeued, avoiding an enter/exit race."* At `daemon.rs:1003-1010`, on
`RestartAssistant`, the same reasoning is spelled out for the stop-then-start pair.

That argument is not specific to the assistant. A fast Stop→Start on the dictation
hotkey — a double-tap, or a hotkey that repeats — runs `on_stop_recording()`
concurrently with `on_start_recording()`, against the same capture slot in the
orchestrator, in whichever order the scheduler picks. The mitigation the author
identified and applied to one pair was not applied to the two structurally identical
pairs beside it.

The consequence is state-dependent rather than a guaranteed fault, which is why this is
S3 and not S2: it needs a tight double-tap to surface, and the orchestrator may
serialise internally on its own lock. Confirming or eliminating that is the first thing
to check.

**Reproduction.** `theory-only` — a reproduction needs either a scripted sub-100 ms
Stop→Start on the real hotkey path or a unit test around the orchestrator's capture
slot, neither of which exists today (`daemon.rs` is at 7.1% coverage, `F-0016`). Per
the Stage-3 gate this must be reproduced or downgraded before any fix is approved.

**Suggested direction.** Two viable directions, and the choice should be deliberate:
either await the `Stop*` arms inline for consistency with their `Start*` partners
(simplest, matches the documented reasoning, risks blocking the event loop on a slow
stop), or keep spawning but make the orchestrator's start/stop pair mutually exclusive
so ordering is enforced where the state actually lives. Whichever is chosen, the
spawn/await decision deserves one comment stating the rule, since three arms currently
carry per-arm justifications and five carry none.

**Blast radius.** Confined to the match, but touches the orchestrator's concurrency
contract if the second option is taken.

## F-B004 — "Pause hotkeys" is a permanent menu item that does nothing, and a test locks it in place

**Severity:** S2 · **Confidence:** high · (also satisfies S4)

**Location:** `crates/fono-tray/src/menu.rs:138`, `crates/fono/src/daemon.rs:1191-1193`,
test at `crates/fono-tray/src/menu.rs:621`

**Observation.** `menu.rs:138` pushes the item unconditionally, in the top-level menu,
third from the top:

```rust
items.push(MenuNode::item("Pause hotkeys", TrayAction::Pause));
```

The handler is:

```rust
TrayAction::Pause => { debug!("tray: Pause hotkeys (not yet implemented)"); }
```

At default verbosity `debug!` is not printed, so clicking it produces no log, no
notification, no icon change, and no state change.

**Why it is wrong.** This is a user-visible defect, not dead code: the item is always
rendered, indistinguishable from the working items around it, and the only feedback a
user gets is that their hotkeys keep firing after they asked them to stop. There is no
"unimplemented" affordance — no greying, no suffix.

The surrounding machinery is complete and unreachable. `TrayState::Paused` exists
(`crates/fono-tray/src/lib.rs:241`), round-trips through the `u8` mapping
(`lib.rs:382`), and has both a tooltip *"Fono — paused"* (`menu.rs:100`) and a grey icon
colour (`menu.rs:115`). A workspace-wide search for `TrayState::Paused` outside the
tray crate's own definition and mapping returns **zero producers** — nothing can ever
put the tray into the state the UI is fully prepared to display.

The test at `menu.rs:621` asserts the exact top-level label list including
`"Pause hotkeys"`, so the menu structure is pinned by a test that never actuates the
item. It passes precisely because it checks the label and not the behaviour, which is
the S4 shape from the rubric.

**Reproduction.** Run the daemon with a tray, click **Pause hotkeys**, then press the
dictation hotkey. Recording starts as normal. `RUST_LOG=debug` shows the
`not yet implemented` line.

**Suggested direction.** Decide between the two honest options rather than leaving the
third. Either implement it — the state, tooltip and icon already exist, so the work is
a flag consulted by the hotkey consumer plus a `TrayState::Paused` transition — or stop
rendering it until it works, and delete the assertion from `menu.rs:621` along with it.
`TrayState::Paused` can stay either way; it costs nothing and is the target of the
implementation.

**Blast radius.** Removal: two lines plus a test line. Implementation: one `AtomicBool`
read in the hotkey consumer and one tray-state transition.

## F-B005 — A poisoned mutex silently reports "no MCP activity", disarming Escape-to-cancel

**Severity:** S3 · **Confidence:** medium

**Location:** `crates/fono/src/daemon.rs:1129`

**Observation.** The action dispatcher reads MCP recursion depth as:

```rust
let depth = mcp_activity_disp.lock().map(|g| g.0).unwrap_or(0);
```

`mcp_activity` is a `std::sync::Mutex<(u32, TrayState)>` (`daemon.rs:1077-1079`). If any
holder of that lock panics, the mutex is poisoned and every later `lock()` returns
`Err` — which `unwrap_or(0)` converts into the sentinel meaning *"no MCP interaction is
active."*

**Why it is wrong.** Zero is not a neutral default here, it is a specific claim about
the world, and it is the claim that suppresses cancellation: with `depth == 0` the
dispatcher takes the non-MCP branch and Escape stops reaching the in-flight
`fono.listen`. The failure is permanent for the process lifetime (poisoning never
clears) and completely silent — no `warn!`, no tray change. The user's symptom is that
Escape stopped cancelling voice input, with nothing in the log at default verbosity to
connect it to the earlier panic.

Choosing `unwrap_or(0)` is defensible for the *tray-state* half of the tuple, where a
wrong guess is cosmetic. It is not defensible for the half that gates a cancellation
path.

**Reproduction.** `theory-only` — requires inducing a panic in a holder of
`mcp_activity`. Whether any holder can actually panic while holding it has not been
traced; that trace is the Stage-3 gate for this finding, and if no holder can panic
this drops to S5.

**Suggested direction.** At minimum log at `warn!` when the lock is poisoned so the
symptom is diagnosable. Better: use the poisoned value rather than discarding it
(`unwrap_or_else(PoisonError::into_inner)`) — the tuple is two plain values and cannot
be torn, so the last-written depth is strictly better information than `0`. Best, if
any holder turns out to be panic-capable, is to hold the depth in an `AtomicU32`, which
has no poisoning semantics at all and matches how it is used.

**Blast radius.** One line for the logging or `into_inner` variants.

## F-B006 — The single-instance guard is a TOCTOU, and losing it double-grabs the hotkey

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono/src/daemon.rs:191-210`, `crates/fono-ipc/src/lib.rs:262-267`

**Observation.** The guard probes for a live daemon and bails if one answers:

```rust
if socket_path.exists() {
    match fono_ipc::connect(&socket_path).await {
        Ok(_) => anyhow::bail!("another fono daemon is already running ..."),
        Err(_) => { /* stale; continue */ }
    }
}
```

The bind that follows is roughly 1,290 lines later (`daemon.rs:1481`), and
`bind_listener` opens with an **unconditional** unlink:

```rust
#[cfg(not(windows))]
if socket.exists() { let _ = std::fs::remove_file(socket); }
```

**Why it is wrong.** The check and the bind are separated by the entire daemon startup
— model preflight, hotkey grab, tray construction, server reconciliation — which on a
cold start with a model download is seconds to minutes, not microseconds. Two daemons
started inside that window both observe no live socket, both proceed, and the second
one's unlink takes the socket away from the first. The first daemon does not notice: it
keeps running, keeps its global hotkey grab and its audio device, and is simply
unreachable over IPC forever after. The user's symptom is one hotkey press starting two
recordings, and `fono toggle` talking to only one of them.

The window is widest in exactly the situation where a double start is most likely —
login, where a systemd user unit and a desktop autostart entry can both fire.

Recording this explicitly because the guard **does** exist and does the common case
correctly; the finding is about the gap between the check and the bind, not a missing
check. (The initial read of `bind_listener` in isolation suggested there was no guard at
all — there is, at `daemon.rs:191`.)

**Reproduction.** `theory-only` for the natural race. Deterministic demonstration:
start one daemon, wait for readiness, then run a second with the guard's `connect`
probe forced to fail; the second binds and orphans the first.

**Suggested direction.** Replace the probe-then-bind pair with a single atomic
acquisition, so there is no window to lose. The direct form is an advisory `flock` on a
lock file next to the socket, held for the process lifetime — `rustix` is already in
the graph, so this is **net-zero on binary size**. A smaller change that shrinks rather
than closes the window is to move the guard immediately before `bind_listener` at
`daemon.rs:1481`; that is worth doing regardless, since nothing between line 191 and
line 1481 depends on the guard having run.

**Blast radius.** Moving the check: two blocks in `run()`. The `flock` version also
touches `fono-ipc`, and needs care on Windows, where named pipes have no on-disk
artefact and `bind_listener` already skips the unlink.

## Unit B — lenses with no findings

- **Lens 2 (error paths):** clean. Fallible startup calls either carry `.context(...)`
  and propagate (`bind_listener` at `daemon.rs:1481`) or log-and-degrade with a
  user-facing `warn!` (hotkey grab at `daemon.rs:306-312`, model preflight at
  `daemon.rs:213-215`). The degraded-mode branch at `daemon.rs:861-881` correctly emits
  `ProcessingDone` for every event that would otherwise strand the FSM outside `Idle` —
  this is careful code.
- **Lens 5 (boundary values):** not applicable in this scope. The two index-taking tray
  arms both validate: `SetWaveformStyle` (`daemon.rs:1344-1347`) and `ToggleLanguage`
  (`daemon.rs:1374-1377`) each use a `let Some(..) else { warn!; continue }` guard.
  `active_provider` (`daemon.rs:605`) maps a missing backend to `u8::MAX` rather than
  panicking.
- **Lens 6 (resource lifetime):** the only finding is the destructor-skipping in
  `F-B001` / `F-B002`. Handles are otherwise held in `run()` locals with correct scope,
  and `AdvertiserHandle` has a real `Drop`.

## Unit B — summary

| ID | Severity | One line |
|---|---|---|
| `F-B001` | S2 | SIGTERM unhandled; shipped systemd units never shut down cleanly |
| `F-B002` | S2 | Tray Quit `process::exit`s past the cleanup two comments promise |
| `F-B003` | S3 | `Stop*` arms spawn, `Start*` arms await — the documented race, unguarded |
| `F-B004` | S2 | "Pause hotkeys" ships, does nothing, and is pinned by a label-only test |
| `F-B005` | S3 | Poisoned mutex silently disarms Escape-to-cancel for MCP |
| `F-B006` | S3 | Guard-to-bind TOCTOU can orphan a running daemon holding the hotkey grab |

**The pattern in this unit is shutdown.** Three of six findings (`F-B001`, `F-B002`,
and the resource half of `F-B006`) are the same underlying gap: the daemon has exactly
one correct teardown path and it is the one only developers use. Fixing `F-B001` and
`F-B002` together — one shutdown channel that SIGTERM, SIGINT and tray Quit all feed —
is a smaller change than fixing either alone, and it makes the existing comments at
`daemon.rs:332-334` and `daemon.rs:346-348` true instead of aspirational.

**On coverage.** Every finding here is in code that `F-0016` measured as unexecuted.
`F-B004` in particular could not survive a single test that actuated the menu item it
asserts the presence of. This is the evidence for the Task 1.11 reordering that put
Unit B first.

---

# Stage 2 — Unit C: IPC request handling and tray helpers

**Scope:** `crates/fono/src/daemon.rs:1810-2116` (`handle_client`,
`handle_mcp_activity_start`, `handle_mcp_activity_end`) and the `*_via_tray` helper
family (`daemon.rs:2297-2925`). Touches `crates/fono-ipc/src/lib.rs` and the three
`crates/fono/src/install/*.rs` shutdown paths where they are the callers.

**Coverage of lenses:** all eight applied. Lens 4 (invariants) produced no separate
finding — the degraded-mode contract is honoured uniformly. Findings cluster in
lenses 1, 3, 6 and 8; lens 8 alone produced three.

## F-C001 — `Request::Shutdown` never answers, so the installer's anti-race wait never runs on any platform

**Severity:** S2 · **Confidence:** high

**Location:** `crates/fono/src/daemon.rs:2030-2032`, `crates/fono-ipc/src/lib.rs:353-358`,
`crates/fono/src/install/linux.rs:1036-1061`,
`crates/fono/src/install/macos.rs:747-764`,
`crates/fono/src/install/windows.rs:136-154`

**Observation.** The handler is an arm of `let resp = match req { … }`:

```rust
Request::Shutdown => {
    std::process::exit(0);
}
```

The arm diverges, so `write_frame(&mut stream, &resp)` at `daemon.rs:2034` is never
reached. But the client half, `request_any`, writes and then *waits for a reply*:

```rust
write_frame(&mut stream, req).await?;
let resp: Response = read_frame(&mut stream).await?;   // lib.rs:356
```

The daemon exits instead of replying, so this `read_frame` returns `Err` (EOF). All
three installers then evaluate:

```rust
timeout(Duration::from_secs(1), request_any(&sockets, &Request::Shutdown))
    .await.ok().and_then(Result::ok).is_some()
```

`request_any` yields `Err` → `Result::ok` → `None` → **`sent == false`**, always, on
exactly the successful path.

**Why it is wrong.** `sent` gates the settle-wait that each installer added on purpose,
and the Linux comment names the precise race it is there to prevent (`linux.rs:1051-1054`):
*"Give the old daemon a moment to release the socket file and tear down its hotkey
grabs before the next autostart races for them."* None of the three ever executes:

| Platform | Dead guard | Line |
|---|---|---|
| Linux | 1.5 s poll until the socket disappears | `install/linux.rs:1055-1060` |
| macOS | 300 ms sleep | `install/macos.rs:762` |
| Windows | 400 ms sleep | `install/windows.rs:152` |

The user-facing line `"asked existing fono daemon to exit"` is also never printed, even
though the daemon did exit — so the install log actively misreports what happened.

Two aggravating factors. First, `process::exit` here does not even unlink the socket
(the tray Quit path at `daemon.rs:1185` at least does that much, see `F-B002`), so the
Linux poll's own exit condition — `sockets.iter().any(|p| p.exists())` — would stay true
for the full 1.5 s even if the branch were entered. Second, on Windows the installer is
about to overwrite the running `.exe`, and Windows holds a lock on a running image; the
400 ms settle is the only thing standing between the shutdown request and a sharing
violation during the copy.

**Reproduction.** Start the daemon, run `fono install`, and observe that
`· asked existing fono daemon to exit` is absent from the output while the daemon has in
fact exited. Instrument `sent` to confirm it is `false`. Equivalently: `fono install`
with the daemon running leaves the stale socket file in place afterwards.

**Suggested direction.** Reply before exiting. The cleanest version folds into the
`F-B002` shutdown channel — write `Response::Ok`, signal the shutdown channel, return
from the handler, and let `run()` unwind normally, which also unlinks the socket and
fires the mDNS goodbye. A minimal standalone fix is to `write_frame(&mut stream,
&Response::Ok).await?` and flush before `process::exit`, but that still skips every
destructor and leaves the socket, so the installer poll would keep spinning for its full
1.5 s. Fixing `F-B002` and this together is strictly less work than fixing this alone.

**Blast radius.** One arm in `handle_client`. The installers need no change once the
daemon replies — their existing logic becomes correct as written.

## F-C002 — One mutex, three different poisoning policies, and the panicking two poison it while holding it

**Severity:** S3 · **Confidence:** medium

**Location:** `crates/fono/src/daemon.rs:2051`, `crates/fono/src/daemon.rs:2090`,
`crates/fono/src/daemon.rs:1129`

**Observation.** `mcp_activity: Arc<std::sync::Mutex<(u32, TrayState)>>` has exactly
three readers in the codebase, and no two agree on what a poisoned lock means:

| Site | Policy | Outcome |
|---|---|---|
| `daemon.rs:2051` (`…_start`) | `.expect("mcp_activity lock poisoned")` | panics the connection task |
| `daemon.rs:2090` (`…_end`) | `.expect("mcp_activity lock poisoned")` | panics the connection task |
| `daemon.rs:1129` (dispatcher) | `.unwrap_or(0)` | silently claims "no MCP activity" (`F-B005`) |

**Why it is wrong.** The two `expect` sites panic *while holding the lock*, which is the
event that poisons a `std::sync::Mutex`. So the failure mode is self-perpetuating: the
first panic inside either critical section poisons the mutex, and from then on every
`McpActivityStart` and `McpActivityEnd` panics on arrival, while the dispatcher —
reading the same mutex three lines of policy away — silently degrades to depth `0`.

The composite end state after one panic is worse than either half suggests: the tray is
stuck amber with no path back to its baseline (`…_end` panics before it can restore),
the Escape grab is never released (`DisableCancel` at `daemon.rs:2100` is downstream of
the panic), and Escape simultaneously stops cancelling because the dispatcher reads `0`.
Nothing logs at default verbosity except the panic itself, in a spawned task whose
`JoinHandle` is dropped at `daemon.rs:1512`.

Whether a first panic is actually reachable has not been traced: the critical sections
call `Tray::state`, `Tray::set_state`, a channel `send`, and `info!`. Any of those
panicking inside a tray backend or subscriber would do it. That trace is the Stage-3
gate; if no holder can panic, this collapses to the S5 "pick one policy" note.

**Reproduction.** `theory-only`. Deterministic demonstration: inject a panic in the
`…_start` critical section, then issue any subsequent `McpActivityStart` and observe
both the repeat panic and the dispatcher's silent `0`.

**Suggested direction.** Pick one policy and apply it at all three sites. Given the data
is `(u32, TrayState)` — two `Copy` scalars that cannot be left in a torn state by a
panic — poisoning carries no safety information here, which is the textbook case for
either `unwrap_or_else(PoisonError::into_inner)` everywhere or moving the depth to an
`AtomicU32` that has no poisoning at all. Whichever is chosen, the three sites should
stop disagreeing; see `F-B005` for the dispatcher half.

**Blast radius.** Three lines, or a small type change confined to `daemon.rs`.

## F-C003 — The cross-process speak lock has no timeout, so one hung client silences every agent

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono/src/daemon.rs:2008-2028`

**Observation.** `Request::McpSpeakAcquire` takes a process-wide mutex and holds it for
the lifetime of the client's socket:

```rust
let _slot_guard = mcp_speak_slot.lock().await;
write_frame(&mut stream, &Response::Ok).await?;
loop {
    match stream.read(&mut buf).await {
        Ok(0) | Err(_) => break,
        Ok(_) => {}
    }
}
```

There is no timeout on the `lock().await`, none on the hold, and no cap on the number of
waiters. `crates/fono-ipc/src/lib.rs` contains no `timeout` or `Duration` anywhere.

**Why it is wrong.** The doc comment at `daemon.rs:1482-1491` argues the design is safe
because *"a crashed MCP server releases the slot via kernel-level socket cleanup."* That
is true for a crash and false for a hang, which is the more common failure for an agent
integration: the process stays alive, the socket stays open, the kernel cleans up
nothing, and the guard is held indefinitely. Because the lock is deliberately global
across every `fono mcp serve` process, one wedged agent takes `fono.speak` away from all
of them, permanently, with no log line and no way to break it short of killing the
holder.

The waiters compound it: each blocked acquire is a spawned task parked on
`lock().await`, and with no bound, agents that retry accumulate tasks for as long as the
holder persists.

**Reproduction.** Connect to the daemon socket, send `Request::McpSpeakAcquire`, read
the `Ok`, then hold the connection open without writing or closing. Every subsequent
`fono.speak` from any process blocks forever.

**Suggested direction.** Bound the hold, not just the acquire — a timeout on
`lock().await` only moves the failure to the waiters while the wedged holder keeps the
slot. Wrapping the read loop in a `tokio::time::timeout` generous enough for the longest
plausible utterance (speech synthesis is seconds, not minutes) and dropping the guard on
expiry with a `warn!` gives the system a way to recover on its own. `tokio::time` is
already in the graph, so this is **net-zero on binary size**.

**Blast radius.** One handler arm. Changes an observable contract — a client that is
merely slow could lose the slot — so the timeout value wants a deliberate choice and a
comment.

## F-C004 — The two hold-until-EOF loops sit 40 lines apart and disagree about stray bytes

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono/src/daemon.rs:1999-2002` versus
`crates/fono/src/daemon.rs:2022-2027`

**Observation.** `handle_client` contains two loops with the same job — hold a resource
until the client closes the connection. They read differently.

`McpActivityHold` (`daemon.rs:1999-2002`) breaks on *any* completed read:

```rust
result = read_half.read(&mut eof_buf) => {
    let _ = result;
    break;
}
```

`McpSpeakAcquire` (`daemon.rs:2022-2027`) breaks only on EOF or error, and its comment
says why: *"a well-behaved client only closes the socket, but a buggy one might write
stray bytes; we keep holding the mutex until EOF / error either way."*

```rust
match stream.read(&mut buf).await {
    Ok(0) | Err(_) => break,
    Ok(_) => {}
}
```

**Why it is wrong.** The hazard the second loop is explicitly hardened against is left
open in the first. A single stray byte on the hold connection makes `read` return
`Ok(1)`, which the Hold loop treats as end-of-span: it falls through to
`handle_mcp_activity_end` (`daemon.rs:2005`), which decrements the depth, restores the
tray from amber, and sends `DisableCancel` — all while the MCP server still believes its
`fono.listen` is running. The user is left mid-listen with the tray showing idle and
Escape no longer wired to cancel, and the eventual real `McpActivityEnd` hits the
`depth == 0` branch at `daemon.rs:2092` and is discarded with a `debug!`.

This is the odd-one-out shape from lens 8, made sharper by the fact that the correct
handling is written out, with justification, in the same function.

**Reproduction.** Open an `McpActivityHold` connection, read the `Ok` ack, then write one
arbitrary byte instead of closing. The tray leaves amber immediately and the Escape grab
is released while the span is nominally still open.

**Suggested direction.** Give the Hold loop the same `Ok(0) | Err(_) => break, Ok(_) =>
{}` discrimination the speak loop already uses. Better still, factor the one correct
loop into a shared `hold_until_eof(read_half)` helper called by both, so the two cannot
drift again.

**Blast radius.** Three lines, or one small helper plus two call sites.

## F-C005 — A connection that sends nothing pins a task for the daemon's lifetime

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono/src/daemon.rs:1824`, `crates/fono-ipc/src/lib.rs`

**Observation.** Every accepted connection begins with an unbounded read:

```rust
let req: Request = read_frame(&mut stream).await?;
```

There is no timeout at the call site and none inside `fono-ipc` — the crate contains no
`timeout` or `Duration` at all. The accept loop spawns one task per connection
(`daemon.rs:1512`) with no cap on concurrent connections and no `JoinHandle` retained.

**Why it is wrong.** A client that connects and never writes a frame parks its task
forever, holding the `Arc` clones of the FSM, orchestrator, registry, tray and paths that
were moved into it at `daemon.rs:1501-1511`. Nothing reaps it, nothing counts it, and
nothing logs it. Repeated connections accumulate tasks and file descriptors until the
process hits its `RLIMIT_NOFILE`, at which point `accept` starts failing — and the accept
arm at `daemon.rs:1500` propagates that error with `?`, terminating `run()` and taking
the daemon down.

The socket is mode `0600` (`fono-ipc/src/lib.rs:279-281`), so this is same-user only and
not a security boundary — it is a robustness gap, reachable by a buggy or half-killed MCP
server as easily as by anything deliberate. That is why it is S3 rather than higher.

**Reproduction.** Open N connections to the IPC socket without writing. Observe the
daemon's fd count and task count rise monotonically and never fall.

**Suggested direction.** Wrap the initial `read_frame` in a `tokio::time::timeout` of a
few seconds — every legitimate client writes its request immediately after connecting, so
the bound is uncontroversial and costs nothing. Note the two long-lived requests park
*after* this first frame (`McpActivityHold`, `McpSpeakAcquire`), so a header timeout does
not conflict with them. Worth pairing with a `warn!` on the accept-error path rather than
`?`, so an fd exhaustion degrades instead of killing the daemon.

**Blast radius.** One wrapper at `daemon.rs:1824`; optionally one changed error path at
`daemon.rs:1500`.

## F-C006 — Twenty-odd unsynchronised read-modify-write cycles on one config file, behind an atomic write that hides them

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono/src/daemon.rs:2881-2925` (`apply_pref_via_tray`) and 12 further
`cfg.save(...)` sites in `daemon.rs`; `crates/fono/src/cli.rs:1199-1224`,
`crates/fono/src/cli.rs:1556`; `crates/fono-core/src/config.rs:2303-2311`

**Observation.** The canonical mutation is load, mutate, save the whole file:

```rust
let mut cfg = fono_core::Config::load(&config_path)?;
mutate(&mut cfg);
cfg.save(&config_path)?;
```

`Config::save` is genuinely atomic — `atomic_write` (`config.rs:2316`) writes a
`NamedTempFile` in the same directory, `sync_all`s it, and renames over the target. But
atomicity applies to the *write*, not to the read-modify-write, and there is no lock,
no generation counter and no compare-and-swap over the pair. Thirteen such cycles exist
in `daemon.rs` alone, plus the web-settings `put_config` hook at `daemon.rs:4146` and
several in `cli.rs`.

**Why it is wrong.** Because each site saves the *entire* `Config`, a lost update does
not lose one field — it silently reverts every field the winner's snapshot was stale
about. Two writers with overlapping load→save windows leave the file matching whichever
saved second, in full.

In-process the tray is safe: the dispatcher at `daemon.rs:1178` processes actions
sequentially and awaits each arm, so two `apply_pref_via_tray` calls cannot interleave.
The exposure is across the three writers that are *not* on that path:

- the web-settings `put_config` hook (`daemon.rs:4146`), on the HTTP server's own task;
- `fono use …` / `fono set …` (`cli.rs:1199-1224`), in a **separate process**;
- the wizard (`crates/fono/src/wizard.rs`), also a separate process.

Cross-process means an in-process mutex would not be sufficient even if one existed. The
realistic sequence is ordinary: a user has the web settings page open, saves it, and in
the same window toggles something from the tray or runs `fono use stt whisper` in a
terminal. One of the two changes vanishes, with no error and no log — the losing write
succeeded, it was simply overwritten.

The atomic write is what makes this worth writing down: it is easy to read
`atomic_write` and conclude config updates are safe, and for durability they are. The
gap is one level up.

**Reproduction.** `theory-only` for the natural race. Deterministic demonstration: pause
inside `apply_pref_via_tray` between `load` and `save`, run `fono use stt whisper` in
another terminal, release. The CLI's change is gone from `config.toml`.

**Suggested direction.** Serialise the whole cycle, not the write. An advisory file lock
held across load→mutate→save is the direct fix and works cross-process, which an
in-process mutex cannot; `rustix` is already in the graph, so **net-zero on binary
size**. A narrower alternative is to funnel every writer through a single daemon-side
IPC request so the CLI and wizard stop writing the file directly — larger, but it also
retires the "restart the daemon to pick this up" class of problem. Either way the
thirteen open-coded cycles in `daemon.rs` want to go through one helper;
`apply_pref_via_tray` is already most of that helper.

**Blast radius.** Wide but shallow — one helper plus mechanical call-site conversion.
The locking version touches `fono-core`.

## Unit C — lenses with no findings

- **Lens 4 (invariants):** the degraded-mode contract (`orchestrator == None`) is honoured
  uniformly. Every request needing an orchestrator matches on it and returns
  `Response::Error` with a consistent message rather than unwrapping — `Reload`
  (`daemon.rs:1877`), `AssistantHoldPress` (`daemon.rs:1895`), `AssistantHoldRelease`
  (`daemon.rs:1902`), `AssistantStop` (`daemon.rs:1909`), `AssistantForget`
  (`daemon.rs:1916`). `Status` (`daemon.rs:1855`) degrades to a descriptive string. No
  gaps found.
- **Lens 5 (boundary values):** no findings. The request enum is closed and the match is
  exhaustive; no request in this unit carries an unvalidated index or length.
- **Lens 7 (false-confidence tests):** the `fono-ipc` tests (`lib.rs:360-383`) are honest
  about their scope — they assert bincode round-trips and say so. They do not claim to
  exercise `handle_client`, and no test in this unit was found asserting behaviour it
  does not reach.

**Worth noting as good code.** `Request::Cancel` (`daemon.rs:1918-1933`) carries a long
comment explaining why it routes through the FSM instead of calling the orchestrator
directly, naming the exact user-visible symptom that shortcut caused (*"the 'F7 twice'
bug"*). That is a defect fixed at the right layer with the reasoning preserved.
`apply_pref_via_tray` correctly pushes the blocking file IO to `spawn_blocking` and
handles all three outcomes including the join error (`daemon.rs:2923`).

## Unit C — summary

| ID | Severity | One line |
|---|---|---|
| `F-C001` | S2 | `Shutdown` never replies; the installer settle-wait is dead on all three platforms |
| `F-C002` | S3 | One mutex, three poisoning policies; two panic while holding and poison it |
| `F-C003` | S3 | Global speak lock has no timeout; a hung client silences every agent |
| `F-C004` | S3 | Two hold-until-EOF loops disagree on stray bytes; one ends the MCP span early |
| `F-C005` | S3 | No timeout on the first frame; a silent connection pins a task forever |
| `F-C006` | S3 | 20+ unsynchronised config read-modify-write cycles behind an atomic write |

**The pattern in this unit is the unbounded wait.** `F-C003`, `F-C005` and the hold half
of `F-C004` are all the same omission: a handler waits on a peer with no time bound, no
cap, and no log when the wait becomes permanent. The daemon is written as though every
peer either behaves or crashes; the failure it does not model is the peer that stays
alive and stops responding.

**`F-C001` is the unit's most actionable finding** — deterministic, reproducible in one
command, affecting all three platforms, and already half-fixed by the shutdown channel
that `F-B002` proposes. Note it is the *third* `process::exit` in the daemon (with
`daemon.rs:1186` and the SIGTERM gap in `F-B001`), and the only one that skips even the
socket unlink.

---

# Stage 2 — Unit A: session orchestrator (capture lifecycle and post-STT text path)

**Scope:** `crates/fono/src/session.rs` (6,787 lines, 27.7% line coverage per `F-0016`).
This pass covers the batch capture lifecycle (`CaptureSession`, `on_start_recording`,
`on_stop_recording`, `on_cancel`) and the post-STT text path (`WordSink`,
`has_sentence_boundary`, the shell-command heuristics, context-rule matching). The
assistant turn path (`on_assistant_*`, `run_assistant_turn`) and the live-dictation
pipeline are deferred — they are large enough to warrant their own pass.

**Coverage of lenses:** all eight applied. Findings in lenses 1, 3, 5, 7 and 8.

## F-A001 — `on_stop_recording` releases the capture slot before it releases the audio device

**Severity:** S2 · **Confidence:** high

**Location:** `crates/fono/src/session.rs:2758` versus
`crates/fono/src/session.rs:2811-2812`; guard at `crates/fono/src/session.rs:2612-2616`

**Observation.** `on_start_recording` holds the slot lock across the whole start — `let
mut slot = self.capture.lock().await;` at `session.rs:2612` binds the guard, so it lives
to the end of the function, and the duplicate-start guard at `session.rs:2613-2616`
is correct under it.

`on_stop_recording` does not. At `session.rs:2758`:

```rust
let taken = self.capture.lock().await.take();
```

the guard is a temporary, dropped at the end of the statement. The slot reads `None`
from this point on — but the capture thread is still alive and still owns the cpal
stream. It is not signalled until `stop_and_drain` runs, 54 lines later, at
`session.rs:2811-2812`.

**Why it is wrong.** Between `session.rs:2758` and `session.rs:2812` the orchestrator
reports "no capture active" while a capture is very much active. A `StartRecording`
arriving in that window passes the `slot.is_some()` guard, reaches
`AudioCapture::new(cap_cfg).start()` at `session.rs:2626`, and opens a **second stream on
the same device** while the first is still open.

Two consequences, both user-visible:

- On a device that does not permit a second stream, `cap.start()` fails, the error path
  at `session.rs:2632` fires, and the user's second recording never starts — they get the
  `notify_recording_failure` toast for what looks like a working microphone.
- Auto-mute inverts. Stop unmutes at `session.rs:2765-2767`; start mutes at
  `session.rs:2647-2649`. If start's mute lands before stop's unmute, the system stays
  **unmuted for the whole new recording** — the feature silently stops working, and the
  user's speakers feed back into the new capture.

The `pipeline_in_flight` guard at `session.rs:2599` does not cover this: the pipeline is
not spawned until `session.rs:2866`, after the drain. The window is precisely where
there is no guard at all.

**This resolves the open question in `F-B003`.** That finding asked whether the
orchestrator serialises start against stop internally, which would have downgraded it.
It does not — the slot is released too early — so `F-B003` stands, and the daemon
spawning `StopRecording` at `daemon.rs:893` while awaiting `StartRecording` at
`daemon.rs:883` is what opens the window in practice. The two findings are one defect
seen from two layers, and either fix closes it.

**Reproduction.** Double-tap the dictation hotkey fast enough that the second press lands
during device teardown. With `RUST_LOG=debug`, a successful reproduction shows
`recording started` before `recording stopped`. The auto-mute variant is easier to see:
enable `auto_mute_system`, double-tap, and observe the sink stays unmuted.

**Suggested direction.** Hold the slot lock across the teardown, so the slot is only
`None` once the device is genuinely free. That means keeping the guard alive over the
`spawn_blocking` at `session.rs:2812` rather than taking and dropping in one statement.
An alternative that avoids holding a lock across an `await` is a three-state slot
(`Idle` / `Active` / `Stopping`) where a start arriving in `Stopping` waits or is
rejected explicitly — more code, but it makes the intermediate state nameable instead of
indistinguishable from idle.

**Blast radius.** One function, plus whatever the chosen shape implies for
`on_cancel` (`session.rs:2882`), which takes the slot the same way.

## F-A002 — `on_start_recording` blocks the async executor on device open, in a function that elsewhere spawn_blocks a 5 ms call

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono/src/session.rs:2638`, `crates/fono/src/session.rs:2641`;
contrast `crates/fono/src/session.rs:2655-2660`

**Observation.** After spawning the capture thread, the async `on_start_recording` waits
for it with a blocking `std::sync::mpsc` receive:

```rust
let buffer = match started_rx.recv() {          // session.rs:2638
    Ok(Ok(b)) => b,
    Ok(Err(e)) => { let _ = join.join(); … }    // session.rs:2641
    …
};
```

Neither `recv()` nor `join()` is wrapped. Both park the tokio worker thread until the
audio device finishes opening — which is the slowest thing in the whole start path,
routinely tens to hundreds of milliseconds on ALSA/PulseAudio, and unbounded if the
device is contended.

**Why it is wrong.** The file makes exactly the opposite call 17 lines later, with a
comment explaining why (`session.rs:2655-2660`): *"Sway/Hyprland/X11 IPC takes ~5 ms; run
it on a blocking thread so we don't hold the async executor."* — and duly uses
`spawn_blocking` for the 5 ms operation while the far slower device open above it runs
inline. The author's stated standard is not met by the code immediately preceding the
statement of it.

The impact is degraded rather than fatal on a multi-threaded runtime — one worker is
lost for the duration — but this is the hot path for every single dictation, and it is
also the path that runs while the daemon's event loop may be awaiting it inline
(`daemon.rs:883`, see `F-B003`).

**Reproduction.** `theory-only` as a user-visible symptom. Directly observable by
instrumenting the elapsed time across `session.rs:2638` on a busy audio device, or by
running with `--features tokio/rt` on a single-threaded runtime, where the block is total.

**Suggested direction.** Move the `recv()`/`join()` pair into the `spawn_blocking` the
function already uses for the focus probe, or make the handshake async by replacing the
`std::sync::mpsc` pair at `session.rs:2618-2621` with a `tokio::sync::oneshot`. The
oneshot version is the smaller change and removes the blocking call rather than relocating
it; `tokio::sync` is already in the graph, so **net-zero on binary size**.

**Blast radius.** The channel type change touches the capture thread closure at
`session.rs:2624-2636` and its two send sites.

## F-A003 — A poisoned buffer or a panicked drain silently yields zero audio, reported to the user as "recording too short"

**Severity:** S2 · **Confidence:** high

**Location:** `crates/fono/src/session.rs:387`, `crates/fono/src/session.rs:2812`,
consumed at `crates/fono/src/session.rs:2830-2831`

**Observation.** Two independent failure paths substitute empty audio for real audio,
neither of them logged as a failure.

`stop_and_drain` (`session.rs:387`):

```rust
let pcm = self.buffer.lock().map(|b| b.samples().to_vec()).unwrap_or_default();
```

A poisoned buffer mutex — i.e. the capture thread panicked while holding it — yields
`vec![]`. And its caller (`session.rs:2812`):

```rust
tokio::task::spawn_blocking(move || session.stop_and_drain()).await.unwrap_or_default()
```

A panic inside the blocking task yields `(vec![], Duration::ZERO)`.

**Why it is wrong.** Both land at `session.rs:2830`:

```rust
if elapsed < MIN_RECORDING || samples.is_empty() {
    warn!("recording too short ({capture_ms} ms); skipping STT");
```

so audio loss is reported to the user as a *user error*. The poisoned-buffer path is the
worse of the two, because `elapsed` is computed from `started_at` at `session.rs:386` and
is therefore correct — the user who spoke for eight seconds is told
`recording too short (8000 ms); skipping STT`, a message that contradicts itself. The
dictation is gone, nothing indicates a fault occurred, and the log line actively points
the reader away from the real cause.

This is the third instance of one anti-pattern already recorded in this audit —
`unwrap_or(0)` at `daemon.rs:1129` (`F-B005`) and the three-way split at `daemon.rs:2051`
/ `:2090` / `:1129` (`F-C002`). In each case a lock or task failure is converted into a
neutral-looking default that is in fact a specific false claim about the world. Here the
false claim is "the user recorded nothing."

**Reproduction.** `theory-only` for the natural case (needs a panic in the capture
thread). Deterministic demonstration: panic inside the capture thread while holding the
buffer lock, then stop the recording and observe the `too short` warning quoting the full
duration.

**Suggested direction.** Distinguish "empty because the user was quick" from "empty
because something broke". Recovering the samples through
`unwrap_or_else(PoisonError::into_inner)` is right on the merits — a poisoned
`RecordingBuffer` still holds every sample written before the panic, so discarding it
throws away recoverable audio — and the `JoinError` at `session.rs:2812` should be
matched rather than flattened, with a distinct user-facing message. At minimum, neither
path should reach the `too short` branch silently.

**Blast radius.** Two lines plus one new message string; no signature changes.

## F-A004 — A single hyphen in dictated prose defeats the non-English shell gate, and the test that covers it has no hyphen

**Severity:** S2 · **Confidence:** high · (also satisfies S4)

**Location:** `crates/fono/src/session.rs:5773-5774`, gate at
`crates/fono/src/session.rs:5755-5757`, test at `crates/fono/src/session.rs:6385-6395`

**Observation.** `gated_builtin_suffix` exists to keep the *transforming* shell-cleanup
prompt away from natural-language dictation in a terminal — the comment at
`session.rs:5723-5729` says so explicitly, naming *"prose in Romanian"* as the case. The
decision funnels through:

```rust
has_shell_syntax(&normalized)
    || starts_with_shell_command(&normalized)
    || (!is_confident_non_english(language) && has_spoken_shell_marker)
```

Only the **third** branch consults the language. `has_shell_syntax` runs first and
unconditionally, and its marker list (`session.rs:5761-5776`) includes two entries that
are not shell syntax at all:

```rust
"--",
" -",
```

**Why it is wrong.** `" -"` matches any space followed by a hyphen — which is ordinary
punctuation in dictated prose in every language the tool supports. `"asta - da"`,
`"well - actually"`, `"the meeting is 3 -5 pm"` all return `true` from
`has_shell_syntax`, so `looks_like_shell_command` returns `true`, so the transforming
shell suffix is applied to prose. Because this is the first branch, the
`is_confident_non_english` protection is bypassed entirely: setting the language to `ro`
does nothing once a hyphen is present.

The test at `session.rs:6385` asserts the protection holds, using the fixture
`"o sa facem un test sa vedem daca sta face din limba romanana, limba inglesa"` — which
contains a comma but no hyphen. It passes, and would continue to pass, while the
mechanism it names is defeated by one character. That is the S4 shape: the test exercises
the gate on the one input class that never reaches the broken branch.

**Reproduction.** Call `gated_builtin_suffix(profile, "asta - da, mergem maine", Some("ro"))`
against the same `kitty` profile the existing test builds. It returns `Some(suffix)`;
the sibling test asserts `None` for hyphen-free Romanian.

**Suggested direction.** `" -"` and `"--"` are trying to detect command flags, so they
should be anchored as such — a token that begins with `-` or `--` **and** is not the only
content, rather than a raw substring anywhere in the line. Worth reconsidering the branch
order too: applying `is_confident_non_english` to the whole decision rather than only the
spoken-marker branch matches what the comment at `session.rs:5723-5729` claims the
function does. Whichever is chosen, the Romanian test wants a hyphen-bearing case added
so the gate cannot silently regress again.

**Blast radius.** Two entries in a `const` list and one predicate; plus test fixtures.
Changes classification behaviour, so it wants the existing `terminal_suffix_is_kept_for_shell_commands`
cases (`session.rs:6398-6416`) re-run — note `"grep -r fono ."`, `"rm -rf target"` and
`"./script.sh --verbose"` currently pass partly *because* of the over-broad markers, and
should pass on `starts_with_shell_command` alone once anchored.

## F-A005 — `window_title_regex` is not a regex, and the reason given for that no longer exists

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono/src/session.rs:5961-5967`, consumed at
`crates/fono/src/session.rs:5949-5952`; `crates/fono/Cargo.toml:191`

**Observation.** The config field is named `window_title_regex`, and users write it in
`config.toml` as a matching rule. The implementation is:

```rust
/// Minimal substring fallback matcher.  We keep `regex` out of `fono`
/// itself (it's already pulled in by `fono-core` for history); for v0.1
/// the simple `contains` semantics are sufficient and avoid a hot-path
/// dependency.
fn regex_lite_match(needle: &str, hay: &str) -> bool {
    hay.to_ascii_lowercase().contains(&needle.to_ascii_lowercase())
}
```

**Why it is wrong.** The field name is a promise the function does not keep, and the
failure is silent in the worst direction: a user who writes a real pattern gets a rule
that never fires, with no error at load time and no log at match time. `^Firefox$`,
`.*\.pdf`, `foo|bar` are all matched *literally* — `^Firefox$` cannot match any window
title, so the rule is simply inert. There is no validation, so nothing tells the user.

The stated justification is also no longer true, and was not true of this crate when
written: `regex` is a **direct dependency of `fono` itself**, declared at
`crates/fono/Cargo.toml:191`, and `cargo tree -p fono -i regex` shows it entering through
both `fono` and `fono-core`. The comment's premise — that using it here would pull
something new in — is false, so honouring the field name is **net-zero on binary size**.
The `for v0.1` scoping note has also outlived its context; the project is at v0.18.1.

**Reproduction.** Set `window_title_regex = "^kitty$"` on a context rule with a matching
window. The rule never fires. Set `window_title_regex = "kitty"` and it does.

**Suggested direction.** Either use `regex` and make the name true — compiling patterns
once at config load rather than per match, which also gives a place to report an invalid
pattern to the user — or rename the field to `window_title_contains` and keep the cheap
semantics. The first is preferable given the dependency is already paid for and rule
matching is not hot (once per utterance). If the field is renamed instead, the old name
needs an alias so existing configs keep working.

**Blast radius.** One function plus a compiled-pattern cache if the regex route is taken;
`fono-core`'s config schema if the rename route is taken.

## Unit A — S5 batch

- **`has_sentence_boundary` only recognises ASCII terminators** (`session.rs:5345-5357`).
  The byte-wise scan is correct and safe for UTF-8 — continuation bytes are ≥ 0x80 and
  can never equal `.`/`!`/`?`, so there are no false positives — but `…` (U+2026), `。`
  (U+3002) and the fullwidth `！`/`？` are not boundaries. For a tool that accepts an
  arbitrary `[general].languages` list this silently disables sentence-boundary flushing
  for CJK dictation. Low impact today given the shipped default languages.
- **`regex_lite_match` lowercases both sides on every call** (`session.rs:5966`),
  allocating two `String`s per rule per utterance. Irrelevant at current rule counts;
  noted only because it disappears for free if `F-A005` takes the regex route.

## Unit A — lenses with no findings

- **Lens 5 (boundary values), text path:** `WordSink` (`session.rs:5369-5420`) verifies
  clean against its own stated contract (`session.rs:5365-5367`). Traced by hand for
  empty input, whitespace-only pushes, leading whitespace before `started`, internal
  runs of `\n`, and multi-byte Unicode whitespace — `rfind(char::is_whitespace)` returns
  a char-start byte index so every slice and `drain` lands on a valid boundary, and the
  push-concatenation-plus-flush identity holds in each case. `normalised_rms`
  (`session.rs:182-189`) guards the empty slice before dividing.
- **Lens 4 (invariants), start path:** the duplicate-start guard at `session.rs:2613` is
  correct and correctly locked; the `pipeline_in_flight` check at `session.rs:2599`
  guards re-entry during the pipeline. The gap is the window between them, which is
  `F-A001` rather than a defect in either guard.

## Unit A — summary

| ID | Severity | One line |
|---|---|---|
| `F-A001` | S2 | Stop frees the capture slot before the device; a second stream can open |
| `F-A002` | S3 | Device open blocks the executor, 17 lines above a comment forbidding exactly that |
| `F-A003` | S2 | Poisoned buffer or panicked drain reports lost audio as "recording too short" |
| `F-A004` | S2 | One hyphen defeats the non-English shell gate; the test fixture has no hyphen |
| `F-A005` | S3 | `window_title_regex` is `contains`; the size justification for that is false |

**`F-A001` is the unit's most important result** because it settles `F-B003`. That finding
was recorded `theory-only` with the explicit caveat that internal serialisation in the
orchestrator would downgrade it. There is none — the slot is released 54 lines before the
device is — so the daemon-level spawn/await split and the orchestrator-level early release
are one defect with two contributing sites.

**The poisoned-lock anti-pattern is now confirmed as systemic**, not local. `F-B005`,
`F-C002` and `F-A003` are the same substitution — a lock or task failure silently becomes
a neutral-looking default that asserts something false — at four sites across two files,
with four different spellings and no shared policy. It is worth treating as one decision
rather than four fixes.

**Deferred from this unit:** the assistant turn path (`on_assistant_hold_press` /
`_release` / `_stop`, `session.rs:2964-3570`), live dictation
(`session.rs:4234-4877`), `run_pipeline` (`session.rs:4878-5314`) and
`stream_cleanup_and_inject` (`session.rs:5447-5663`). These are roughly 2,400 lines and
the least-covered part of the file; they want their own pass.

---

# Stage 2 — Unit A2: session orchestrator (pipeline, streaming injection, cancellation)

**Scope:** the ~2,400 lines of `crates/fono/src/session.rs` deferred from Unit A —
`spawn_pipeline` (`session.rs:4034-4160`), `stream_cleanup_and_inject`
(`session.rs:5447-5663`), the live-dictation teardown (`session.rs:4440-4877`), the
assistant turn path (`session.rs:3288-3810`), and `on_cancel` (`session.rs:2881-2946`).
Extends into `crates/fono-hotkey/src/fsm.rs` where the cancellation contract is decided.

**Coverage of lenses:** all eight applied. Lens 1 (cancellation) dominates and produced
the unit's only S2. Lens 8 supplied the comparison that makes it a defect rather than a
design choice.

## F-A006 — Batch dictation is the only pipeline that cannot be cancelled, and the only one that types into your window

**Severity:** S2 · **Confidence:** high

**Location:** `crates/fono-hotkey/src/fsm.rs:348`, `crates/fono/src/session.rs:4082`,
`crates/fono/src/session.rs:2881-2882`

**Observation.** Once the user releases the dictation hotkey, the batch pipeline runs STT,
then the polish LLM, then injects. Nothing can stop it, for two independent reasons.

*The event never arrives.* The FSM has a `CancelPressed` arm for every other state —
`Recording` (`fsm.rs:207`), `LiveDictating` (`fsm.rs:227`), `AssistantRecording`
(`fsm.rs:249`), `AssistantThinking` (`fsm.rs:258`), `AssistantSpeaking` (`fsm.rs:264`),
`AssistantLive` (`fsm.rs:313`), `McpDriven` (`fsm.rs:334`). There is **no**
`(State::Processing, HotkeyAction::CancelPressed)` arm, so the press falls through to
`(current, _) => current` at `fsm.rs:348` and is discarded without emitting
`HotkeyEvent::Cancel`.

*And there is nothing to cancel with.* `spawn_pipeline` launches the work as a statement
(`session.rs:4082`):

```rust
tokio::spawn(async move { … });
```

The `JoinHandle` is dropped immediately. No `AbortHandle` is stored on the orchestrator
and none is passed in, so no caller could abort this task even if it received the event.
`on_cancel` (`session.rs:2882`) only takes the *capture* slot — which stop already
emptied — finds `None`, skips its whole body, and sends `ProcessingDone`.

**Why it is wrong.** Every sibling pipeline in this file solves this, three different
ways, all of them working:

| Pipeline | Cancellation mechanism | Location |
|---|---|---|
| Live dictation | stores `run_join`, calls `.abort()` | `session.rs:2935` |
| Assistant turn | dedicated `Arc<Notify>` | `session.rs:3714` |
| MCP tool call | FSM barge-in → `McpToolCancelled` | `fsm.rs:334-342` |
| **Batch dictation** | **none** | — |

The batch path is the odd one out, and it is the worst one to omit: it is the default
dictation flow, it is the path with the longest uninterruptible tail (STT plus an LLM
polish pass is seconds, longer on a cold model or a cloud provider), and it is the one
that ends by *typing into whatever window has focus*. A user who realises mid-processing
that they dictated into the wrong window, or dictated something they did not mean to
send, has no way to stop it — Escape is silently ignored and the text arrives anyway.

The comment at `session.rs:2953-2954` shows the asymmetry was noticed for the assistant
("gates on its own cancellation `Notify`") without the batch gap being drawn out.

Related but distinct from `F-B003`/`F-A001`: those concern *restarting* during teardown.
This is about stopping work already handed off.

**Reproduction.** Configure a slow polish model (or a cloud provider) so the processing
phase lasts several seconds. Dictate, release, then press the cancel hotkey repeatedly
during processing. The text is injected regardless. `RUST_LOG=debug` shows no
`HotkeyEvent::Cancel` emitted.

**Suggested direction.** Two halves, and both are needed — either alone leaves the
feature broken. Add a `(State::Processing, HotkeyAction::CancelPressed)` arm to the FSM
that emits `Cancel` and returns to `Idle`; and give `spawn_pipeline` the same treatment
the live path already uses — retain the `AbortHandle`, store it beside
`pipeline_in_flight`, and abort it from `on_cancel`. A `Notify` checked at phase
boundaries (post-STT, post-polish, pre-inject) is the gentler variant and avoids aborting
mid-injection, which matters because a partially injected line is worse than a fully
injected one. Whichever is chosen, `pipeline_in_flight` must be cleared on the cancel
path or the next dictation is refused by the guard at `session.rs:2599`.

**Blast radius.** One FSM arm plus its test, one stored handle, one branch in
`on_cancel`. The FSM change is trivially testable — `fsm.rs` is among the best-covered
files in the workspace.

## F-A007 — Streaming injection silently degrades to one-shot for any non-Latin script

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono/src/session.rs:5469`, `crates/fono/src/session.rs:5345-5357`

**Observation.** The first-flush gate in `stream_cleanup_and_inject` is a conjunction:

```rust
if has_sentence_boundary(&buf) && has_enough_text_for_language_guard(buf.trim()) {
```

The two conjuncts disagree about whether non-Latin scripts exist.
`has_enough_text_for_language_guard` counts non-ASCII alphabetic characters explicitly —
it was written to work for any script. `has_sentence_boundary` (`session.rs:5345-5357`)
scans bytes for `.`, `!`, `?` only.

**Why it is wrong.** A model transcribing Japanese, Chinese, Hindi or Arabic emits `。`,
`！`, `？`, `।` or `؟` — never the ASCII forms. The gate therefore never opens, the
buffer grows for the entire utterance, and the first flush happens only at the
end-of-stream drain. Streaming injection — the feature's whole point, text appearing as
you speak — is off for those users, with no warning, no log line, and no configuration
that explains it. It looks like the feature is slow rather than disabled.

This upgrades the S5 note recorded in Unit A. On its own, missing `…` was cosmetic; as
one half of a gate whose other half is script-aware, it disables a headline feature for
every language `[general].languages` accepts but the terminator list does not. The
divergence between the two conjuncts is what makes this an oversight rather than a
scoping decision.

The byte-wise scan itself remains correct — UTF-8 continuation bytes are ≥ 0x80 and can
never equal an ASCII terminator, so there are no false positives, and the decimal-point
exclusion (`session.rs:6661`) works. The defect is the alphabet, not the algorithm.

**Reproduction.** Set `[general].languages` to include `ja` or `zh`, enable streaming
injection with a streaming-capable backend, and dictate a multi-sentence utterance. No
text appears until the utterance ends. Repeat in English: text appears at the first
sentence.

**Suggested direction.** Extend the terminator set to the Unicode sentence terminators
the supported languages actually use — at minimum `。` `！` `？` `．` (U+3002, FF01, FF1F,
FF0E), `।` (Devanagari danda), `؟` (Arabic question mark) and `…` (U+2026). That means
iterating `chars()` rather than bytes, which costs nothing at these lengths and removes
the need to reason about continuation bytes at all. The existing tests at
`session.rs:6657-6661` should keep passing unchanged, and want CJK cases added alongside.

**Blast radius.** One function and its test block. No signature or config change.

## Unit A2 — lenses with no findings

- **Lens 6 (resource lifetime), live teardown:** clean and notably careful.
  `on_cancel`'s live branch (`session.rs:2908-2943`) aborts the silence task, signals the
  capture thread, joins both threads on `spawn_blocking`, aborts the run task rather than
  awaiting it (with a comment saying why — *"we don't care about the partial
  transcript"*), drains, hides the overlay, and clears `pipeline_in_flight`. Every handle
  is accounted for and the ordering is correct. This is the model the batch path in
  `F-A006` should follow.
- **Lens 3 (concurrency), assistant session slot:** clean. The comment at
  `session.rs:3715-3718` documents holding the state lock across spawn-and-store
  specifically so the pump cannot clear the slot before the handle is recorded —
  *"Resolves the spawn/store race."* This is the exact hazard `F-A001` found unguarded in
  the batch capture slot, correctly handled here. Direct evidence that the batch gap is an
  oversight, not a different risk assessment.
- **Lens 4 (invariants), `pipeline_in_flight`:** the batch path pairs
  `store(true)` at `session.rs:4081` with `store(false)` at `session.rs:4145` inside the
  same spawned task, so the flag cannot leak on any pipeline outcome including a panic in
  a nested phase. The live path's four store sites (`session.rs:4486`, `:4560`, `:4569`,
  `:4601`, `:4871`) were traced and each early-return path clears it.

## Unit A2 — summary

| ID | Severity | One line |
|---|---|---|
| `F-A006` | S2 | Batch dictation cannot be cancelled once processing starts; every sibling can |
| `F-A007` | S3 | ASCII-only sentence terminators disable streaming injection for non-Latin scripts |

**The unit's finding is an absence, not a mistake.** Both findings are cases where the
right thing exists elsewhere in the same file and was not carried across: three working
cancellation mechanisms beside a pipeline with none, and a script-aware predicate
conjoined with an ASCII-only one. Unit A2 adds no new anti-pattern — it adds two more
instances of the sibling-divergence shape that has now produced findings in all three
units (`F-B003`, `F-C004`, `F-A002`, `F-A006`, `F-A007`).

**Note on the batch pipeline's coverage.** `spawn_pipeline` and
`stream_cleanup_and_inject` sit in the 27.7%-covered region measured by `F-0016`.
`F-A006`'s FSM half, by contrast, is in one of the best-covered files in the workspace —
and is still missing, because the tests assert the transitions that exist rather than
probing for the one that does not. Worth remembering when reading coverage numbers as a
proxy for confidence.

---

# Stage 2 — Unit D: assistant runtime

**Scope:** `crates/fono/src/assistant.rs` (3,715 lines) and `crates/fono/src/wake.rs`
(781 lines). Corresponds to plan Task 2.3 ("Unit C — Assistant runtime"); the ledger
letter differs because `F-C` was already spent on the IPC unit. See the unit-letter
mapping note at the end of this section.

**Coverage of lenses:** all eight applied. Lens 1 (cancellation) produced both findings.

## F-D001 — The assistant's cancel signal is lossy by construction, so Escape can leave it talking

**Severity:** S2 · **Confidence:** high (mechanism) / medium (frequency)

**Location:** `crates/fono/src/assistant.rs:236`; contrast
`crates/fono/src/assistant.rs:250` and the comment at
`crates/fono/src/assistant.rs:261-262`

**Observation.** Turn cancellation is signalled with:

```rust
pub fn stop_current_turn(&mut self) {
    if let Some(notify) = self.current_turn.take() {
        notify.notify_waiters();          // assistant.rs:236
    }
    if let Some(pb) = &self.playback { pb.stop(); }
}
```

`Notify::notify_waiters` wakes only the tasks **already registered** on a `notified()`
future at the instant it is called. It stores no permit, so a call that finds no
registered waiter is discarded — a later `notified()` will not complete because of it.
`notify_one`, by contrast, stores a permit and cannot be lost.

The file knows this. Eleven lines below, the live-session handle uses the other one, and
its doc comment states the reason outright (`assistant.rs:261-262`): *"Signalled (via
`notify_one`, so the wake is never lost) to tear the live pump down."*

**Why it is wrong.** The turn pump is only a registered waiter while it is parked on one
of its `notify.notified()` select arms — the delta loop (`assistant.rs:930`), the TTS
synth select (`assistant.rs:1766`), and the drain poll (`assistant.rs:1270`). Between
those it runs a stretch of synchronous work with no waiter registered: the sentence
splitter (`assistant.rs:1068`), trace emission, metrics updates, and the loop bookkeeping
at `assistant.rs:1079-1114`. A cancel arriving in one of those windows is dropped
outright.

The consequence is not a delay, it is a lost cancellation. `pb.stop()` still runs, so the
user hears the current audio cut — which reads as "Escape worked". But the pump never
learns, so it keeps consuming deltas, keeps synthesising the remaining sentences, and
keeps enqueueing them onto the same playback handle. The assistant falls silent and then
**resumes speaking the rest of its reply**, which is a worse outcome than not cancelling
at all, because the user has already moved on. `aborted_mid_stream` also stays `false`,
so the turn is recorded in history as clean.

Two mitigations exist and neither closes it. The drain poll is saved by `pb.stop()`
making `is_idle()` true within 100 ms, and the comment at `assistant.rs:1250-1253`
correctly claims *"either path wakes this loop"* — that reasoning is sound but applies
only to the drain, not to the generation loop above it. And `notify_triggered` at
`assistant.rs:1108`/`:1112` is positioned exactly where it would catch a lost wake, but
always returns `false` (see `F-D002`).

**Reproduction.** `theory-only` — hitting the synchronous window requires timing. A
deterministic demonstration: insert a `std::thread::sleep` in the sentence loop between
`synth_and_enqueue` returning and the next iteration, then cancel during that sleep; the
turn runs to completion and speaks the remainder.

**Suggested direction.** Use `notify_one()` at `assistant.rs:236` so the wake is stored,
exactly as the live path already does — a one-word change that makes the loss impossible
rather than unlikely. Note this only works because there is a single consumer per
`current_turn` `Notify`, which is the case. The structurally cleaner option is
`tokio_util::sync::CancellationToken`, which is level-triggered rather than
edge-triggered and would make `notify_triggered` a real probe — but `tokio-util` is
**not currently in the dependency graph**, so that carries a binary-size cost and needs
sign-off; `notify_one` does not.

**Blast radius.** One method call. Behaviour becomes strictly more cancel-responsive; no
API change.

## F-D002 — A function named like a cancellation guard is hardcoded to `false`, at two call sites that read as guards

**Severity:** S4 · **Confidence:** high

**Location:** `crates/fono/src/assistant.rs:3242-3250`, called at
`crates/fono/src/assistant.rs:1108` and `crates/fono/src/assistant.rs:1112`

**Observation.**

```rust
fn notify_triggered(_notify: &Arc<Notify>) -> bool {
    // … this helper exists for symmetry but always returns false.
    false
}
```

Both call sites are written as bail-outs:

```rust
if notify_triggered(&notify) { break; }
```

**Why it is wrong.** This is the false-confidence shape applied to code rather than to a
test. A reader scanning the sentence loop sees a cancellation check after each synthesis
and reasonably concludes the loop is cancel-responsive between sentences. It is not —
both branches are statically dead, and the compiler cannot warn because the parameter is
used only in name.

The doc comment is candid about the mechanism and even names the intended future fix, but
it draws the wrong conclusion: *"The select arms above already cover cancellation."* They
cover the awaits, not the synchronous stretches between them — which is precisely the gap
`F-D001` exploits. So the one place a lost wake could be recovered is occupied by a
function that cannot recover it, and the comment explaining why is the reason nobody has
looked again.

The `_notify` parameter is never read, so this also passes an `Arc` clone reference for
no purpose.

**Reproduction.** Not applicable — statically evident.

**Suggested direction.** Delete the function and both call sites, which makes the absence
of a between-sentence cancellation check visible instead of disguised; or make it real,
which follows automatically if `F-D001` moves to `notify_one` (a stored permit can be
probed with `now_or_never` on `notified()`) or to a `CancellationToken` (`is_cancelled`).
Deleting is the honest minimum and should not wait on the larger decision.

**Blast radius.** Three lines removed.

## Unit D — lenses with no findings

- **Lens 3 (concurrency), live-mode floor handoff:** clean, despite using `Relaxed`
  ordering on two cross-thread flags. `give_floor_to_model` (`assistant.rs:2966-2974`)
  and `give_floor_to_user` (`assistant.rs:2978-2986`) write `idle_armed` and `mic_muted`
  in opposite orders, so a reordering is observable in principle — but the consumer
  checks the mute gate **first** and returns early (`assistant.rs:2747-2751`), before it
  ever reads `armed` (`assistant.rs:2761`). Every interleaving of the two writes was
  traced and each either returns at the gate or reaches a benign state; no torn
  combination causes an auto-close during the model's turn. Worth a comment rather than a
  change.
- **Lens 5 (boundary values), pure helpers:** `truncate` (`assistant.rs:3205-3211`) is
  correctly char-based, not byte-based, so it cannot panic on a multi-byte boundary —
  the usual defect in this shape is absent. `goertzel_mag` (`assistant.rs:1677-1694`)
  guards `n == 0` and `sample_rate == 0`, and its power term
  `s1² + s2² − coeff·s1·s2` matches the standard formulation. `resample_linear`
  (`assistant.rs:2003-2019`) guards zero rates and empty input and clamps its tail read
  with `unwrap_or(a)`. `tts_audio_band_windows` guards a zero peak before dividing
  (`assistant.rs:1731`).
- **Lens 2 (error paths), mid-stream failures:** the stream-error branch
  (`assistant.rs:951-981`) classifies the error and raises a user-facing notification
  only for the four classes a user can act on (auth, payment, network, terms), rather
  than notifying on everything. This is the right discrimination.

**Worth noting as good code.** `classify_tool_outcome` (`assistant.rs:3213-3238`) takes
the executor's boolean verdict as authoritative and consults the prose *only* to
distinguish kinds of failure, with a comment recording the exact defect that motivated
it — a Home Assistant success payload ending in `"failed": []` being keyword-matched as a
failure and telling the user their light had not come on while it visibly had. That is a
bug fixed at the right layer with the evidence preserved.

## Unit D — summary

| ID | Severity | One line |
|---|---|---|
| `F-D001` | S2 | `notify_waiters` is lossy; a dropped cancel leaves the assistant speaking on |
| `F-D002` | S4 | `notify_triggered` is hardcoded `false` at two sites that read as guards |

**The two findings are one defect and its disguise.** `F-D001` opens a window where a
cancel is lost; `F-D002` sits in that window looking like the thing that would catch it.
Neither is visible without reading both, which is why the lens pass matters more here
than in units where a single line is wrong on its face.

**This is the third cancellation finding in three consecutive units** — `F-A006` (batch
dictation cannot be cancelled at all), `F-C003` (the speak lock has no timeout), and now
`F-D001`. Each pipeline in this codebase implements cancellation independently and each
gets it wrong differently. Flagged for the Task 2.15 cross-unit synthesis.

## Ledger unit-letter mapping

The ledger's letters diverged from the plan's when the daemon unit was split into the
event loop and the IPC surface. Recorded here once so citations resolve both ways:

| Ledger unit | Plan task | Subject |
|---|---|---|
| A, A2 | 2.1 | Session orchestration |
| B | 2.2 | Daemon event loop |
| C | 2.2 (split) | IPC request handling and tray helpers |
| D | 2.3 | Assistant runtime |
| E | 2.4 | State machines |
| F | 2.5 | Audio real-time path |
| G | 2.6 | Untrusted input decoders |
| H | 2.7 | Network servers |
| I | 2.8 | FFI and `unsafe` |
| J | 2.9 | Local inference backends |
| K | 2.10 | Config, paths, persistence |
| L | 2.11 | Cloud provider backends |
| M | 2.12 | Injection and platform integration |
| N | 2.13 | Rendering and overlay |
| O | 2.14 | Update, download, bench, CLI |

---

# Stage 2 — Unit E: state machines

**Scope:** `crates/fono-hotkey/src/fsm.rs` (630 lines), `crates/fono-hotkey/src/listener.rs`
(616 lines), and the `KeyHeldFlags` atomics shared between the listener thread and the
orchestrator. Plan Task 2.4.

**Method note.** The unit's central question — "are all transitions reachable?" — is not
answerable by reading the FSM alone, so every `HotkeyAction` variant was traced to its
producers across the workspace. That trace is what produced both findings; neither is
visible from `fsm.rs` in isolation.

## F-E001 — An entire FSM state and its three transitions are unreachable, and the feature they describe is implemented a second time elsewhere

**Severity:** S3 · **Confidence:** high · (also satisfies S4)

**Location:** `crates/fono-hotkey/src/fsm.rs:328-347`, `crates/fono-hotkey/src/fsm.rs:125`,
`crates/fono-hotkey/src/fsm.rs:165-167`; dead consumers at
`crates/fono/src/daemon.rs:819-822`, `crates/fono/src/daemon.rs:855-856`,
`crates/fono/src/daemon.rs:1040-1042`

**Observation.** `State::McpDriven` is entered by exactly one arm:

```rust
(State::Idle, HotkeyAction::McpToolStarted(tool)) => { … State::McpDriven { tool } }
```

`HotkeyAction::McpToolStarted` and `HotkeyAction::McpToolDone` have **no producer
anywhere in the workspace** — a full-tree search finds them only in `fsm.rs` itself.
Nothing dispatches either action, so the state is never entered.

Everything downstream is therefore dead: the barge-in arm (`fsm.rs:334-342`), the
normal-completion arm (`fsm.rs:344-347`), and the three `HotkeyEvent` variants they emit
(`McpToolStarted`, `McpToolCancelled`, `McpToolDone`). Three separate consumer groups in
the daemon exist to handle those events and can never run — the tray-state mapping
(`daemon.rs:819-822`), the overlay mapping (`daemon.rs:855-856`), and the explicit
no-op arm in the hotkey consumer (`daemon.rs:1040-1042`). `wake.rs:633-635` tests
`should_listen` against three `McpDriven` states that cannot occur.

**Why it is wrong.** The functionality is not missing — it is implemented twice, and the
wrong copy is wired. The live implementation runs over IPC: `Request::McpActivityStart` /
`McpActivityEnd` increment and decrement the `mcp_activity` depth counter
(`daemon.rs:2051`, `:2090`), and the cancel decision is taken from that counter in the
action dispatcher (`daemon.rs:1129`).

That matters because the audit has already found the wired copy to be the fragile one.
`F-B005` records that a poisoned `mcp_activity` mutex silently reports depth `0` and
disarms Escape; `F-C002` records three conflicting poisoning policies on the same mutex,
two of which panic while holding it. The dead copy has none of those problems — a state
machine state cannot be poisoned, its transitions are exhaustive by construction, and
`fsm.rs` is among the best-covered files in the workspace. The design that was reasoned
about carefully is the one that never runs.

This is also why it is not merely dead code: a reader auditing MCP cancellation finds the
FSM arms first, concludes barge-in is handled by the state machine, and never examines
the depth counter that actually decides.

**Reproduction.** Not applicable — statically evident. Confirmable by adding a
`debug_assert!(false)` inside the `McpToolStarted` arm and exercising every MCP tool
path; it never fires.

**Suggested direction.** Decide which design is authoritative and delete the other. If the
FSM is chosen, the MCP server's activity hooks dispatch `McpToolStarted`/`McpToolDone`
instead of (or in addition to) the IPC depth counter, and `F-B005`/`F-C002` are resolved
by deletion rather than by fixing a mutex policy — which is the stronger outcome. If the
depth counter is chosen, remove `State::McpDriven`, both actions, the three
`HotkeyEvent` variants, the three daemon consumer groups and the `wake.rs` test rows;
that is a meaningful size and complexity reduction. What should not persist is both.

**Blast radius.** Deletion: ~40 lines across three files plus test rows. Wiring the FSM:
larger, but it subsumes two open S3 findings.

## F-E002 — `HotkeyAction` carries two more variants nothing produces, one of them guarding a wildcard transition

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono-hotkey/src/fsm.rs:323`, `crates/fono-hotkey/src/fsm.rs:139`,
`crates/fono-hotkey/src/fsm.rs:149`, `crates/fono-hotkey/src/fsm.rs:237-240`

**Observation.** Two further variants have no dispatcher:

- **`ProcessingStarted`** — referenced outside `fsm.rs` only by a translation test at
  `daemon.rs:6386-6387`. Nothing sends it.
- **`AssistantReleased`** — zero references outside `fsm.rs`. The listener converts a long
  assistant press-release into a second `AssistantPressed` (`listener.rs:461`), which is
  caught by the toggle arm at `fsm.rs:244`, so the push-to-talk stop works — via a
  different arm than the one named for it. `(AssistantRecording, AssistantReleased)` at
  `fsm.rs:237-240` never runs, and `HotkeyEvent::StopAssistant` is emitted only from
  `fsm.rs:245`.

**Why it is wrong.** `AssistantReleased` is ordinary dead weight, and would be an S5 on
its own. `ProcessingStarted` is not, because of the shape of its arm:

```rust
(_, HotkeyAction::ProcessingStarted) => State::Processing,
```

This is the FSM's **only** source-state-agnostic transition. Every other arm names its
origin, which is what makes the machine auditable. As long as nothing dispatches the
action the wildcard is harmless — but it is a loaded trap for whoever wires it up. A
single `ProcessingStarted` sent while the FSM is in `AssistantLive` would move it to
`Processing` **without emitting `ExitAssistantLive`**, orphaning the live pump: the mic
keeps streaming, the realtime session stays open, and the subsequent `ProcessingDone`
lands the FSM in `Idle` with a full-duplex conversation still running and no state
tracking it. The same wildcard swallows `McpDriven` without emitting `McpToolCancelled`,
and `Recording` without emitting `Cancel` or `StopRecording`.

The variant is also `#[non_exhaustive]`-exported from a library crate, so a future caller
outside this repo can dispatch it.

**Reproduction.** Not applicable for the dead path. The trap is demonstrable in a unit
test: drive the FSM to `AssistantLive`, dispatch `ProcessingStarted`, and observe
`State::Processing` with no event emitted on the channel.

**Suggested direction.** Delete both variants along with the wildcard arm and the
`AssistantReleased` arm; `ProcessingDone` alone is sufficient, since every producer
already pairs with it (49 references). If `ProcessingStarted` is wanted later, reintroduce
it with explicit source states — `(State::Recording(_) | State::LiveDictating(_),
ProcessingStarted)` — so the machine stays exhaustively auditable. Either way the
translation test at `daemon.rs:6386` goes with it.

**Blast radius.** ~15 lines across `fsm.rs` and one daemon test row.

## Unit E — S5 batch

- **Forty-two editor backup files sit in `crates/`** — 21 `*.rs~` and 21
  `.*.un~` undo files, including `crates/fono/src/session.rs~`,
  `crates/fono/src/assistant.rs~`, `crates/fono-core/src/config.rs~` and
  `crates/fono-overlay/src/renderer.rs~`. They are correctly gitignored
  (`.gitignore:34`), so this is not a repository-hygiene defect. It is an **audit and
  tooling hazard**: a workspace-wide `grep` matches stale copies of files that have since
  changed, and during this unit a search returned line numbers from `assistant.rs~` that
  do not correspond to `assistant.rs`. Any future automated sweep over the working tree
  will need to exclude them explicitly. Worth a `find crates -name '*~' -delete` before
  the next mechanical pass.

## Unit E — lenses with no findings

- **Lens 4 (invariants), transition exhaustiveness:** apart from the wildcard in `F-E002`
  and the missing `(Processing, CancelPressed)` already recorded as `F-A006`, every arm
  names both its source state and its action, and the `(current, _) => current` fallback
  at `fsm.rs:348` is the correct default for a UI state machine — an unexpected key press
  should be ignored, not fault.
- **Lens 3 (concurrency), `KeyHeldFlags`:** clean. The three `AtomicBool`s are written by
  the listener thread and read by the orchestrator, always as independent flags with no
  data published alongside them, so `Ordering::Relaxed` is correct — there is nothing for
  an acquire/release pair to order. The plan flagged these as a possible lock-free channel
  with lost-update risk; they are not a channel, and each flag has a single writer.
- **Lens 1 (cancellation), listener state:** `map_event`'s `Role::Cancel` arm
  (`listener.rs:474-486`) clears both press timestamps *and* both held flags, with a
  comment naming the two symptoms that motivated each — a spurious synthesised stop/start
  pair from a late key-up, and suppressed pondering on the next session because Cancel
  delivers no key-up for the dictation key. This is the correct and complete handling.

**Worth noting as good code.** `on_assistant_stop` (`crates/fono/src/session.rs:3500-3560`)
carries the audit's best comment: it explains that skipping the batch capture teardown
left an orphaned silence-watch task that committed three seconds later and emitted a
synthetic `AssistantPressed`, producing a phantom `AssistantRecording` with no overlay
while the still-occupied slot made every later wake-word fire log a duplicate-start
warning. The bug, the mechanism, the two user-visible symptoms and the fix, in one place.

## Unit E — summary

| ID | Severity | One line |
|---|---|---|
| `F-E001` | S3 | `State::McpDriven` is unreachable; MCP cancellation is implemented twice, wired once |
| `F-E002` | S3 | Two producer-less actions, one guarding the FSM's only wildcard transition |

**Both findings are the same measurement:** four of the seventeen `HotkeyAction` variants
have no producer, and the FSM arms built for them are unreachable. The machine reads as
more capable than it is, which is the specific risk in a component whose whole purpose is
to be the auditable description of the system's behaviour.

**`F-E001` is worth reading against `F-B005` and `F-C002`.** Those two findings describe
a fragile MCP-cancellation implementation; this one shows a robust alternative already
exists in the codebase and is simply not connected. That reframes them from "fix the
mutex policy" to "pick the design", which is a better Stage-3 question.

---

# Stage 2 — Unit F: audio real-time path

**Scope:** `crates/fono-audio/` (7,071 lines) — `capture.rs`, `playback.rs`,
`resample.rs`, `vad.rs`, `envelope.rs`, `silence_watch.rs`, `stream.rs`, `trim.rs`, and
the `speaker.rs` / `wake_registry.rs` asset-fetch edge. Plan Task 2.5.

## F-F001 — The resampler never flushes, so every capture loses its last ~20 ms and a resampler error drops a whole chunk in silence

**Severity:** S2 · **Confidence:** high

**Location:** `crates/fono-audio/src/resample.rs:33-46`; consumers at
`crates/fono-audio/src/capture.rs:571`, `crates/fono-audio/src/playback.rs:570-580`,
`crates/fono-stt/src/openai_streaming.rs:496`, `crates/fono/src/cli.rs:2615`

**Observation.** `Resampler::process` buffers input and emits only whole 1024-sample
windows:

```rust
pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
    self.leftover.extend_from_slice(input);
    let mut out = Vec::new();
    while self.leftover.len() >= self.chunk {
        let slice = &self.leftover[..self.chunk];
        if let Ok(result) = self.inner.process(&[slice.to_vec()], None) { … }
        self.leftover.drain(..self.chunk);
    }
    out
}
```

The type exposes no `flush`, `finish` or `drain` method — `process` is its only public
operation besides `new`. Whatever is in `leftover` when the struct is dropped is
discarded.

**Why it is wrong.** Two separate defects share these thirteen lines.

*The tail is always lost.* A recording ends at an arbitrary sample count, so on average
half a chunk and at worst 1,023 device-rate samples never reach the pipeline. At the
common 48 kHz that is up to **21.3 ms**, at 44.1 kHz **23.2 ms**. This is not an
occasional glitch: the resampler is constructed only when `device_rate != target_rate`
(`capture.rs:568-572`), which is the normal case for consumer hardware, so essentially
every dictation on a 48 kHz microphone is clipped at the end. Twenty milliseconds is
where a final unvoiced consonant lives — the release of a `/t/`, `/k/` or `/s/` — so the
symptom is a transcript that occasionally drops the last phoneme, which reads as an STT
accuracy problem rather than an audio one.

The same omission affects the OpenAI streaming wire path (`openai_streaming.rs:496`) and
playback (`playback.rs:570-580`), where the lost tail is the end of the synthesised
utterance.

*A failed chunk vanishes without trace.* `if let Ok(result) = self.inner.process(…)`
discards the `Err` entirely — no `warn!`, no counter, no propagation — and then drains
the input anyway. If `rubato` ever fails a window, 1,024 samples disappear from the middle
of a recording and nothing anywhere records that it happened. The surrounding code is
otherwise diligent about logging degradation (`capture.rs:574` warns on cpal stream
errors), so this is an outlier rather than a house style.

The test at `resample.rs:53-61` asserts only that some output is produced from 2,048
samples of silence. It cannot detect either defect: it never checks the output length
against the expected ratio, and it never ends the stream.

**Reproduction.** Feed exactly 2,048 samples at 48 kHz→16 kHz and compare the output
length with the expected ~683. Then feed 2,500 and observe the output is unchanged —
the extra 452 samples are retained and never emitted. For the user-visible form, dictate
a word ending in a hard consonant and compare the transcript against the same phrase with
trailing silence appended.

**Suggested direction.** Add a `flush(&mut self) -> Vec<f32>` that zero-pads `leftover`
up to one chunk, processes it, and truncates the result to the exact
`leftover.len() × ratio` output samples so the padding does not leak audible silence —
then call it at end-of-capture, end-of-utterance and end-of-stream in each of the four
consumers. Note the capture-side resampler currently lives inside the cpal callback
closure (`capture.rs:598-631`) and is dropped with the stream, so it has no reachable
owner at stop time; giving the flush a home is the larger half of this change.
Separately, replace `if let Ok(...)` with a match that logs the error at `warn!`.

**Blast radius.** One new method in `resample.rs`, plus a lifetime change for the capture
resampler and one call site in each of four consumers.

## F-F002 — The live-conversation auto-close reimplements the silence watch without its hysteresis, and copies two of its constants

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono/src/assistant.rs:2739-2741` and
`crates/fono/src/assistant.rs:2759-2781`; original at
`crates/fono-audio/src/silence_watch.rs:20-59` and
`crates/fono-audio/src/silence_watch.rs:159-210`

**Observation.** `fono-audio` ships a considered silence-driven auto-close. `SilenceWatch`
tracks a state machine with two separate confirmation windows —
`DEFAULT_SPEECH_CONFIRM_ARM_MS = 100` to arm and `DEFAULT_SPEECH_CONFIRM_RESUME_MS = 200`
to leave `Pondering` — and the doc comment at `silence_watch.rs:36-38` states why the
second is larger: *"because impulse noises (mouse clicks, breaths) sustain voiced"*.

The live-conversation path does not use it. `LiveFrameProcessor::on_frame` implements the
same feature inline (`assistant.rs:2759-2781`) with no confirmation window in either
direction, and redeclares two of the watch's constants as private associated items:

| Constant | `fono-audio` | `assistant.rs` |
|---|---|---|
| Pondering lead-in | `DEFAULT_PONDERING_VISUAL_MS = 1_000` (`silence_watch.rs:28`) | `PONDERING_VISUAL_MS = 1_000.0` (`assistant.rs:2739`) |
| Silence gap | `DEFAULT_SILENCE_GAP_DB = 12.0` (`silence_watch.rs:29`) | `SILENCE_GAP_DB = 12.0` (`assistant.rs:2741`) |

**Why it is wrong.** The relative-threshold logic was copied and the hysteresis was not,
so the two paths behave differently in exactly the conditions the hysteresis exists for.
In the inline version a **single** frame above the gap calls `end_pondering()` and resets
`silence_ms` to zero (`assistant.rs:2770-2773`). A keyboard clack, a chair creak or a
breath therefore restarts the entire auto-close countdown from scratch. In a normally
noisy room the live conversation never closes itself, and the user has to reach for the
hotkey — which is precisely the interaction full-duplex mode exists to avoid.

The duplicated constants are the secondary problem: tuning the gap in `silence_watch.rs`,
where the documented defaults live, silently leaves live mode on the old value.

**Reproduction.** Enter live-conversation mode with `auto_stop` configured, stop
speaking, and tap the desk once per second. The session does not auto-close. The batch
silence watch under the same stimulus arms and fires, because a single impulse does not
satisfy its 200 ms resume window.

**Suggested direction.** Use `SilenceWatch` in `LiveFrameProcessor` instead of the inline
accumulator — it already takes an `EnvelopeSnapshot` and a `frame_ms`, which is exactly
what `on_frame` has at `assistant.rs:2764-2766`, so the call shape matches and the two
local constants disappear with it. This is a case where the shared component is a strict
superset of the copy, so the change removes code rather than adding it.

**Blast radius.** ~20 lines in `assistant.rs`; no change to `fono-audio`.

## Unit F — lenses with no findings

- **The `fono-audio → fono-download` edge is clean**, which was the plan's specific
  concern for this unit. Both `fetch_model` entry points (`speaker.rs:831`,
  `wake_registry.rs:344`) are `async` and are reached only from daemon startup preflight
  (`daemon.rs:213`) and the tray-driven model fetch (`daemon.rs:3718`). The capture hot
  path is a synchronous cpal callback (`capture.rs:598-631`) which cannot invoke them.
  Model fetching provably cannot occur on the capture path.
- **Lens 5 (boundary values), envelope and silence watch:** clean and carefully reasoned.
  `SilenceWatch::push` (`silence_watch.rs:159-168`) explicitly handles the
  `voiced_frames == 0` case with a comment explaining that an unpopulated `voiced_rms`
  reads as −140 dBFS and would make every frame look loud by comparison. `WebRtcVadStub`
  (`vad.rs:37`) guards the empty frame with `len().max(1)`. The `EnvelopeConfig` alphas
  are computed once from the first frame's duration (`envelope.rs:120-124`) rather than
  assumed.
- **Lens 8 (sibling divergence), the pinning sentinel:** `speaker.rs` and
  `wake_registry.rs` use the same `is_pinned` predicate to opposite effect, and the
  contrast is instructive. `speaker.rs:841-846` treats an unpinned asset as *"do not
  fetch"* and degrades AS-Norm to plain cosine scoring — safe. `wake_registry.rs` treats
  an unpinned asset as *"fetch without verifying"* — the defect already recorded as
  `F-0008`. That the safe interpretation exists in the sibling module strengthens
  `F-0008`'s suggested direction.

**Worth noting as good code.** The `envelope.rs` / `silence_watch.rs` pair is the best
engineering read in the audit so far. Using a relative gap against an asymmetric
attack/release follower rather than an absolute dBFS threshold is the right call, and the
module comment (`silence_watch.rs:9-11`) says so in one sentence: it *"self-calibrates
across mic / gain / room"*. The separate arm and resume windows show the failure modes
were thought through rather than discovered.

## Unit F — summary

| ID | Severity | One line |
|---|---|---|
| `F-F001` | S2 | Resampler has no flush; every capture loses up to 21 ms, and a failed chunk vanishes silently |
| `F-F002` | S3 | Live mode reimplements the silence watch without hysteresis and duplicates its constants |

**Both findings share a cause: a good component with a missing edge.** The resampler is
correct for steady-state streaming and wrong only at the boundary nobody wrote; the
silence watch is well designed and simply not called by the one path that most needs it.
Neither is a design error — both are the seam between a component and its lifecycle,
which is the same seam `F-A001` (capture slot released before the device) and `F-B001`
(no destructor on the real exit paths) sit on.

---

# Stage 2 — Unit G: untrusted input decoders

**Scope:** every decoder that turns bytes from another process or another host into
in-memory structures — `crates/fono-net-codec/src/frame.rs` (Wyoming framing, LAN-facing),
`crates/fono-ipc/src/lib.rs` (`read_frame` / `write_frame`, local socket), and the
web-settings HTTP surface in `crates/fono-net/src/web_settings/mod.rs`. Plan Task 2.6.

**Why this unit matters more than its size suggests.** It is the only code in the product
that parses bytes the user did not produce. `crates/fono-core/src/config.rs:58-60`
documents the Wyoming listener as accepting `0.0.0.0`, RFC1918 and link-local binds, so
`frame.rs` is reachable from the LAN whenever a user turns the server on.

## F-G001 — The Wyoming header-length limit is checked after the whole line is in memory, so a peer with no newline can exhaust RAM

**Severity:** S2 · **Confidence:** high

**Location:** `crates/fono-net-codec/src/frame.rs:92-99`

**Observation.** `Frame::read_async` opens with:

```rust
let mut line = String::new();
let n = reader.read_line(&mut line).await?;
if n == 0 { return Err(FrameError::Truncated("header line")); }
if line.len() > MAX_HEADER_LINE_BYTES { return Err(FrameError::HeaderTooLong(line.len())); }
```

`AsyncBufReadExt::read_line` appends to `line` until it sees `\n` or reaches EOF. It takes
no limit. The bound at `frame.rs:97` therefore cannot run until `read_line` has already
returned — which, for a peer that never sends a newline, is never.

**Why it is wrong.** `MAX_HEADER_LINE_BYTES` (1 MiB, `frame.rs:27`) exists precisely to
stop a peer dictating an allocation, and it is placed one statement too late to do it. A
connection that writes `{` and then a continuous byte stream with no `\n` grows `line`
without bound: 1 MiB, 100 MiB, until the allocator fails or the OOM killer takes the
daemon. The error type `HeaderTooLong` is well designed and unreachable in the case that
matters — it can only fire when a *cooperative* peer sends an over-long line and
terminates it.

Three factors set the severity. The surface is off-host whenever
`[server.wyoming].enabled` is on with a non-loopback bind, which is the documented and
supported configuration for using Fono as a Home Assistant speech backend. There is no
connection cap and no accept-rate limit anywhere in `crates/fono-net/` — a search for
`Semaphore` returns nothing — so an attacker is not limited to one concurrent
allocation. And the daemon dying takes the user's global hotkeys with it.

The same `read_async` is used by the *client* side against remote Wyoming servers, so a
malicious or compromised peer the user connects to has the same reach.

**Reproduction.** With `[server.wyoming].enabled = true`, connect to the port and write
bytes continuously without ever sending `\n`. The daemon's RSS tracks the bytes sent.
Contrast a 2 MiB line **with** a trailing newline, which correctly returns
`HeaderTooLong`.

**Suggested direction.** Bound the read rather than the result — wrap the reader in
`AsyncReadExt::take(MAX_HEADER_LINE_BYTES as u64 + 1)` for the header read, or read
byte-wise into a capped buffer and fail as soon as the cap is passed. The existing check
should stay as the cooperative-peer path; the point is that it needs a partner that fires
before the allocation. Worth pairing with a connection cap on the accept loops, which
`F-C005` also asks for on the IPC side.

**Blast radius.** One function in `frame.rs`; the error type already exists.

## F-G002 — A 100-byte header reserves 64 MiB before a single payload byte arrives

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono-net-codec/src/frame.rs:136-140`

**Observation.** Once the header validates, the payload buffer is allocated at the
declared size and only then filled:

```rust
let mut payload = Vec::new();
if payload_length > 0 {
    payload.resize(payload_length, 0);
    read_exact_or_truncated(reader, &mut payload, "payload").await?;
}
```

`payload_length` is peer-supplied and bounded by `MAX_PAYLOAD_BYTES = 64 MiB`
(`frame.rs:31`), so the check at `frame.rs:113` passes for anything up to that.

**Why it is wrong.** This is bounded, which is why it is S3 and not S2, but the bound is
per-connection and the amplification ratio is roughly 600,000:1 — a ~110-byte header
line declaring `payload_length: 67108864` causes a 64 MiB zeroed allocation that is held
until the peer either sends 64 MiB or disconnects. With no connection cap (see `F-G001`),
sixteen such headers reserve a gigabyte from an attacker's ~2 KiB of traffic.

The declared-versus-delivered gap is the crux: the code trusts the declaration to size an
allocation, then discovers the truth. `read_exact_or_truncated` handles the short case
correctly and returns `Truncated` — the memory has simply already been committed by then.

**Reproduction.** Send a valid header with `payload_length: 67108864` and no payload
bytes. Observe the daemon's RSS rise by 64 MiB and stay there until the connection is
closed.

**Suggested direction.** Read the payload incrementally into a `Vec` that grows as bytes
actually arrive — `reader.take(payload_length as u64).read_to_end(&mut payload)` gives the
same result, the same 64 MiB ceiling, and the same `Truncated` semantics, while making the
memory proportional to bytes delivered rather than bytes claimed. Also worth asking
whether 64 MiB is the right ceiling: the largest legitimate payload is one utterance of
16 kHz mono PCM, so ~2 MiB covers a minute of speech.

**Blast radius.** Four lines.

## F-G003 — The local IPC decoder has no length limit at all, while its LAN-facing sibling has three

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono-ipc/src/lib.rs:181-193`; contrast
`crates/fono-net-codec/src/frame.rs:27-31` and the sender-side check at
`crates/fono-ipc/src/lib.rs:205`

**Observation.** `read_frame` is:

```rust
let mut len_buf = [0u8; 4];
stream.read_exact(&mut len_buf).await?;
let len = u32::from_be_bytes(len_buf) as usize;
let mut buf = vec![0u8; len];
stream.read_exact(&mut buf).await?;
```

There is no constant to compare `len` against — `fono-ipc` declares no maximum of any
kind. A peer-controlled `u32` therefore sizes the allocation directly, up to 4 GiB, and
the buffer is zeroed before any payload byte is read.

**Why it is wrong.** The asymmetry is the finding. The *write* side of the same file
bounds itself — `u32::try_from(bytes.len()).context("frame too large")` at `lib.rs:205` —
so the sender validates and the receiver does not. And the sibling decoder in the same
workspace, written for the same job, gets it right three times over: a header bound, a
data-block bound and a payload bound, each checked before its allocation. One decoder in
this repository demonstrates the correct pattern and the other does not use it.

The socket is mode `0600` (`lib.rs:279-281`), so this is not a privilege boundary and the
realistic trigger is a buggy or half-killed MCP server rather than an attacker — which is
what holds it at S3. But it compounds directly with `F-C005`: that finding records that
connections have no timeout and no cap, so a loop of connections each declaring 4 GiB is
reachable without the peer ever sending a payload byte, and the daemon has no defence at
either layer.

**Reproduction.** Connect to the IPC socket and send the four bytes `FF FF FF FF`, then
nothing. The daemon attempts a 4 GiB allocation.

**Suggested direction.** Add a `MAX_FRAME_BYTES` const and check `len` against it before
allocating, mirroring `frame.rs:110-115`. A few hundred KiB is generous — the largest
legitimate `Request`/`Response` is a status blob or a transcript. As in `F-G002`, growing
the buffer as bytes arrive is better than sizing it from the declaration. The sender-side
`try_from` at `lib.rs:205` should use the same constant so the two halves agree.

**Blast radius.** One const and two checks in `fono-ipc`.

## Unit G — lenses with no findings

- **The web-settings HTTP surface is clean**, and is the model the other two decoders
  should follow. It runs on hyper rather than a hand-rolled parser, wraps every body in
  `http_body_util::Limited` with an explicit cap (`web_settings/mod.rs:1035-1037`), keeps
  a separate larger cap for audio uploads with a comment justifying it as loopback-only
  (`mod.rs:66`), and clamps the user-supplied `?limit=` query parameter against its own
  maximum with the reason stated — *"so a hand-crafted URL cannot ask the daemon"*
  (`mod.rs:73`). Every input that can size an allocation is bounded at the point of use.
- **Lens 2 (error paths), `frame.rs`:** the error taxonomy is genuinely good.
  `FrameError` distinguishes `Truncated(&'static str)` with the field name,
  `HeaderTooLong`, `DataTooLong`, `PayloadTooLong` and `MissingType`, so a failure says
  which limit or which field caused it. `read_exact_or_truncated` correctly maps
  `UnexpectedEof` to `Truncated` and passes every other IO error through unchanged.
- **Lens 5 (boundary values), `frame.rs` parsing:** the header parse is defensive in the
  right places — a non-object `data` is normalised to an empty object rather than
  rejected or unwrapped (`frame.rs:118-121`), missing lengths default to `0`
  (`frame.rs:107-109`), and `\r\n` from naive peers is tolerated (`frame.rs:101-103`).
  The `as usize` casts on `data_length` and `payload_length` are safe on 64-bit and both
  values are bounds-checked immediately after.

## Unit G — summary

| ID | Severity | One line |
|---|---|---|
| `F-G001` | S2 | Header limit checked after `read_line` completes; a newline-less peer exhausts RAM |
| `F-G002` | S3 | 64 MiB reserved from a ~110-byte header before any payload byte arrives |
| `F-G003` | S3 | IPC decoder has no length cap; its LAN-facing sibling has three |

**All three findings are the same question asked at three points: does the code allocate
what the peer claims, or what the peer delivers?** In every case it allocates the claim.
`F-G001` is the severe one only because the claim is unbounded there; the other two are
the same mechanism with a ceiling on it.

**The unit is unusual in having a correct exemplar inside it.** `frame.rs` shows
`fono-ipc` what a bounds check looks like, and the web-settings server shows both of them
what bounding at the point of use looks like. `F-G003` in particular needs no design work
— it needs the pattern from the file next door.

**Compounding note for Stage 3.** `F-G001`, `F-G002` and `F-G003` all worsen sharply
without a connection cap, and `F-C005` records that neither accept loop has one. A single
`Semaphore` on each accept loop would reduce all four from unbounded to bounded, and is
probably the highest-leverage single change available in the network surface.

---

# Stage 2 — Unit H: network servers

**Scope:** `crates/fono-net/` (5,717 lines) — the Wyoming server
(`wyoming/server.rs`), the LLM inference server (`llm_server/`), the shared inbound-auth
primitive (`auth.rs`), the web-settings server (`web_settings/mod.rs`), and mDNS
discovery (`discovery/`). Plan Task 2.7. The decoder these servers sit on was covered in
Unit G.

**Method note.** The unit's question is "does each listener enforce what its
configuration promises?" That is answered by tracing every configuration field to its
point of use, not by reading the servers. Both findings come from fields that are
resolved, plumbed and stored but never read.

## F-H001 — The Wyoming server's bearer token is resolved, plumbed and stored, and never checked

**Severity:** S1 · **Confidence:** high

**Location:** `crates/fono-net/src/wyoming/server.rs:63-67` (declared),
`crates/fono/src/daemon.rs:3639-3640` (resolved),
`crates/fono/src/daemon.rs:3663` (passed), `crates/fono-core/src/config.rs:1841-1846`
(documented) — no read site exists

**Observation.** A workspace-wide search for `auth_token` in `wyoming/server.rs` returns
exactly two lines: the field declaration at `server.rs:67` and `auth_token: None` in the
`Default` impl at `server.rs:139`. There is no comparison, no extraction from a frame, no
rejection path — the field is written and never read.

Every other link in the chain is complete. The config field exists and is documented
(`config.rs:1841-1846`), the daemon resolves it from the environment
(`daemon.rs:3639-3640`), and passes it into `WyomingServerConfig` (`daemon.rs:3663`). The
plumbing runs the entire distance and stops one statement short of doing anything.

The doc comment states the guarantee in the present tense
(`config.rs:1841-1844`): *"Optional pre-shared bearer token reference, resolved through
`secrets.toml` / env. Empty = no auth. Wyoming v1 has no in-band auth, so this is
checked out of band before the protocol handshake."* Nothing is checked, out of band or
otherwise. `server.rs:63-64` repeats the claim: *"plumbed for the Fono …"*.

**Why it is wrong.** This is the only finding in the audit where a documented **security
control is entirely absent**, and it sits on the one listener the project documents as
LAN-facing. `config.rs:1835-1837` invites the user to `bind = "0.0.0.0"` to serve every
interface — the supported configuration for using Fono as a Home Assistant speech
backend — and `auth_token_ref` is presented as the control that makes that safe.

The only protection that does work is `loopback_only`, and the daemon derives it from the
bind address (`daemon.rs:3638`): `cfg.bind == "127.0.0.1" || cfg.bind == "::1"`. So it is
`true` exactly when the socket is already unreachable from the LAN, and `false` in every
case where a check would matter. A user who widens the bind and sets a token believes
they have swapped one control for another; they have removed the only one.

The consequence is an unauthenticated LAN service that performs speech-to-text and
text-to-speech on the user's hardware, advertises the configured model names and
languages over mDNS, and — via Unit G's findings, which apply to the same listener — can
be driven to exhaust the daemon's memory. The two failures compose: the surface that has
no authentication is the surface with the unbounded header read (`F-G001`).

Contrast `auth.rs`, which is exemplary: it fails closed without a verifier
(`auth.rs:142-144`), verifies a presented token even from loopback so a wrong key is
rejected rather than waved through (`auth.rs:65-72`), documents its three rules, and has
eight unit tests covering every combination. The LLM server and the web-settings server
both use it. The Wyoming server does not, and nothing marks the omission.

**Note on the rubric.** S1 is defined as *"data loss, hang, crash, wrong output reaching
the user, or secret leak"* — it has no security axis, because when it was written none of
the findings needed one. This finding is rated S1 on the grounds that an absent
authentication control on a LAN-facing listener is at least as serious as the categories
listed, not because it matches one of them. Worth adding a security clause to the rubric
before Stage 3 triage.

**Reproduction.** Set `[server.wyoming] enabled = true`, `bind = "0.0.0.0"`,
`auth_token_ref = "FONO_WYOMING_TOKEN"`, export that variable, and restart. From a second
host, connect and send a `describe` frame with no credential of any kind. The server
answers with its `info` frame and will accept transcription work.

**Suggested direction.** The mechanism to use already exists in the same crate. Since
Wyoming v1 has no auth event, the check has to be out of band as the comment intends —
the natural place is immediately after accept in `handle_connection`
(`server.rs:357-378`), requiring the first frame to carry the token in its `data` map
when `auth_token.is_some()`, and closing the connection otherwise. Routing the decision
through `fono_net::auth::decide` keeps one policy across all three listeners and inherits
its fail-closed behaviour and its tests. Whatever is chosen, the comparison must be
constant-time — `subtle` is **not** in the dependency graph, but a hand-rolled
constant-time byte compare is a few lines and adds nothing to the binary.

Until it is implemented, the honest interim is to make the daemon refuse to start a
non-loopback Wyoming listener, or log a `warn!` at startup, exactly as the LLM server
already does for its own weaker case at `daemon.rs:3821-3826`.

**Blast radius.** One check in `handle_connection`, one new frame field, and a
documentation correction. No protocol change for clients that send nothing when no token
is configured.

## F-H002 — Two of three server tokens ignore `secrets.toml`, and the one that fails open is the LAN-facing one

**Severity:** S2 · **Confidence:** high

**Location:** `crates/fono/src/daemon.rs:3640`, `crates/fono/src/daemon.rs:3819`;
contrast `crates/fono/src/daemon.rs:3997` and `crates/fono-stt/src/factory.rs:492`

**Observation.** Three server-side token resolutions live in one file and two of them
bypass the project's secret resolver:

| Consumer | Resolution | Line |
|---|---|---|
| Wyoming server | `std::env::var(&cfg.auth_token_ref).ok()` | `daemon.rs:3640` |
| LLM legacy-token migration | `std::env::var(&cfg.auth_token_ref).ok()` | `daemon.rs:3819` |
| Web settings server | `secrets.resolve(&cfg.auth_token_ref)` | `daemon.rs:3997` |

`Secrets::resolve` (`fono-core/src/secrets.rs:54-59`) checks `secrets.toml` first and
falls back to the environment, so it is a strict superset of `std::env::var`. The daemon
already loads it at `daemon.rs:119`. The outbound Wyoming *client* also uses the resolver
(`fono-stt/src/factory.rs:492`), so within the same feature Fono reads its own token
correctly when connecting out and incorrectly when listening.

**Why it is wrong.** `fono keys add` writes to `secrets.toml`, so that is where a user
who follows the documented workflow puts the token. For the Wyoming listener, that
resolves to `None`, which the server treats as "no auth configured" — and given
`F-H001`, the outcome is the same either way today. It matters for whichever fix
`F-H001` takes: implementing the check without also fixing the resolution produces a
listener that silently ignores a token the user correctly configured, which is a worse
failure than the current one because the user has been given a reason to trust it.

The LLM migration path at `daemon.rs:3819` fails in the safe direction — a token in
`secrets.toml` is simply not migrated into an API key, and remote callers are denied —
but it fails silently, and the warning at `daemon.rs:3821-3826` will fire telling the
user no keys exist without explaining that their configured token was not found.

The divergence is invisible at the call sites because both spellings are one line and
look equally deliberate.

**Reproduction.** Run `fono keys add FONO_WYOMING_TOKEN <value>`, set
`auth_token_ref = "FONO_WYOMING_TOKEN"`, and restart with the variable **not** exported.
`WyomingServerConfig.auth_token` is `None`. Repeat for `[server.web]` and it is `Some`.

**Suggested direction.** Use `secrets.resolve` at both sites — `secrets` is already in
scope at `daemon.rs:119` and the change is one call each. Worth grepping for
`env::var(&cfg.` as a class: any config field named `*_ref` is by convention a secret
reference and should never be resolved with a bare `env::var`.

**Blast radius.** Two lines.

## Unit H — lenses with no findings

- **`auth.rs` is the strongest module read in this audit.** It fails closed with no
  verifier, treats a presented credential as authoritative regardless of origin so a bad
  key from loopback is rejected rather than trusted, documents why loopback without a
  token is admitted (bootstrap lockout), treats an empty bearer as "not presented" so a
  bare `Authorization: Bearer ` header does not deny, and has eight tests covering every
  cell of the truth table. `is_loopback` is derived from the real peer socket address
  (`wyoming/server.rs:348-352`), never from a `Host` or `X-Forwarded-For` header, so it
  cannot be spoofed. The `F-H001` gap is that this module is not called by one listener,
  not that it is wrong.
- **Lens 2 (error paths), the cloud proxy:** `forward_chat` (`llm_server/proxy.rs:61-89`)
  builds a fresh outbound request rather than forwarding the inbound one, so the client's
  `Authorization` header is never relayed upstream and the server's own key is attached
  explicitly. Header laundering — the usual defect in a proxy of this shape — is absent
  by construction.
- **Lens 6 (resource lifetime), Wyoming connections:** `handle_connection` wraps every
  frame read in `tokio::time::timeout(IDLE_TIMEOUT, …)` (`server.rs:378-381`), so an idle
  peer is reaped. Note this does **not** mitigate `F-G001`: a peer streaming bytes
  continuously is never idle, so the timeout cannot fire while the header buffer grows.
  The one defence present is defeated by an active peer rather than a passive one.

## Unit H — summary

| ID | Severity | One line |
|---|---|---|
| `F-H001` | S1 | The Wyoming bearer token is never checked; the LAN listener has no auth at all |
| `F-H002` | S2 | Wyoming and the LLM migration read tokens with `env::var`, ignoring `secrets.toml` |

**Both findings are the same failure at two depths.** `F-H002` is the token not being
found; `F-H001` is the token not being used even when it is found. Fixing `F-H002` alone
changes nothing observable, and fixing `F-H001` alone produces a listener that rejects
users who configured the token the documented way. They are one change.

**The audit's first S1, and it was invisible from the server file.** Reading
`wyoming/server.rs` end to end shows a well-structured listener with an idle timeout, a
loopback guard, and an `auth_token` field in its config struct. The defect only appears
when the field is traced to a read site that does not exist. Every prior unit's findings
were visible from the code; this one required the absence to be measured. That argues for
making "every config field has a read site" a mechanical sweep rather than a reading
exercise — see the Task 2.15 synthesis.

**Compounding with Unit G.** The listener with no authentication is the same listener
with the unbounded header read (`F-G001`), the 64 MiB pre-allocation (`F-G002`), and no
connection cap. Any Stage-3 work on the network surface should treat
`F-G001`/`F-G002`/`F-H001` as one hardening pass rather than three tickets.

## F-H003 — Mechanical sweep: every config field traced to a read site; one is inert

**Severity:** S5 · **Confidence:** high

**Method.** `F-H001` was found by tracing one config field to a read site that did not
exist, which is a mechanical question and should not depend on a reviewer noticing. All
169 `pub` fields across the `Config` tree in `crates/fono-core/src/config.rs` were
extracted and searched for any textual reference in the other 271 Rust files in the
workspace.

**Result.** **168 of 169 have at least one reference outside `config.rs`.** The
exception is:

- **`McpServer.mirror_to_stdout`** (`crates/fono-core/src/config.rs:1991-1993`) —
  documented as *"Whether to mirror spoken text to stdout during `fono.speak` calls. Off
  by default; useful when tuning the agent preset."* There is no read site anywhere.
  Setting it has no effect.

Worth noting the feature is probably better left unimplemented than implemented as
described: the MCP server is stdio-only (`config.rs:1978`), so its stdout **is** the
JSON-RPC transport. Writing spoken text there would corrupt the protocol stream. Either
delete the field, or implement it against stderr and correct the doc comment — but not
against stdout.

The sibling field two lines below is correctly enforced: `listen_max_seconds` is applied
as a real ceiling at `crates/fono-mcp-server/src/tools/listen.rs:134-135`, clamping both
the agent-supplied value and the default.

**Why the sweep is worth keeping.** `F-H001` was a security control with a complete
plumbing chain and no terminus, and it survived because reading the server file makes the
field look used. The sweep costs seconds and answers the question directly. It is a
candidate for a `tests/check.sh` gate — the project already enforces SPDX headers and
comment hygiene the same way (see `F-0001`, `F-0010`).

**Caveat on scope.** The sweep is textual, so it proves a field is *mentioned*, not that
it is *honoured*. `auth_token` would have passed it (it is mentioned in `daemon.rs` and
in `server.rs`'s struct). A stronger version would require a read through a value —
`cfg.field` or a destructuring bind — rather than any mention. That variant was run
across all 3,195 struct fields in the workspace and returns 214 candidates, but the
signal is poor: the great majority are serde wire DTOs whose fields exist only to be
serialised (`ChatReq.temperature`, `ModelEntry.object`, the `*Report.ran_at` set), which
are correctly write-only. Narrowing it to `Config`-tree types only is the version worth
automating.

---

# Stage 2 — Unit I: FFI and `unsafe`

**Scope:** all 140 `unsafe` sites across 19 files, concentrated in
`crates/fono-core/src/vk_loader_shim.rs` (32), `crates/fono-overlay/src/backends/windows.rs`
(18), `crates/fono-core/src/brain_tap.rs` (17), the two `llama_local.rs` backends (15),
`crates/fono-core/src/vulkan_probe.rs` (9), `crates/fono-tray/src/backend_macos.rs` (8),
and the Win32 focus probes in `crates/fono-inject/src/focus.rs` (6). Plan Task 2.8.

**Headline: this is the strongest unit in the audit.** No memory-safety defect was found.
The `unsafe` in this codebase is concentrated where it belongs — C ABI boundaries — and
the hardest of it is the best-documented code in the project.

## F-I001 — The Vulkan shim's re-entrancy safety rests on an unstated linker property

**Severity:** S3 · **Confidence:** low

**Location:** `crates/fono-core/src/vk_loader_shim.rs:351-368`,
`crates/fono-core/src/vk_loader_shim.rs:382`

**Observation.** The shim defines `vkGetInstanceProcAddr` itself with `#[no_mangle]`
(`vk_loader_shim.rs:381-382`) so that ggml's link-time reference resolves to Fono's
forwarder rather than to `libvulkan.so.1`. On first call the forwarder enters
`loader()`, which is a `OnceLock::get_or_init` (`vk_loader_shim.rs:352-353`) that
`dlopen`s the real loader and then calls `device_available`
(`vk_loader_shim.rs:276-323`) — which asks the *real* loader to create and destroy a
throwaway Vulkan instance, all still inside the initialiser.

**Why it is a risk.** `OnceLock::get_or_init` is documented as deadlocking or panicking
if the initialiser re-enters it. So the shim is sound only if nothing reached from
`vkCreateInstance` inside the real loader can call back into Fono's
`vkGetInstanceProcAddr`. A Vulkan loader that resolves its own internal
`vkGetInstanceProcAddr` reference through the PLT would be interposed by an executable
that exports that symbol, and the first GPU-accelerated inference would hang in the
`OnceLock` rather than falling back to the CPU.

**The mitigating factor, which is why this is `low` confidence.** A Rust executable does
not place `#[no_mangle]` symbols in `.dynsym` unless it is linked with `-rdynamic` /
`--export-dynamic`, and nothing in the build configuration appears to request that. If
the three `vk*` symbols are not dynamically exported, interposition is impossible and the
re-entrancy cannot occur — the symbols exist only for the static link against ggml, which
is exactly what the module docs describe. `libvulkan.so.1` is also commonly built with
`-Bsymbolic-functions`, which would prevent it independently.

The finding is recorded not because a defect was demonstrated but because the module's
soundness depends on a property nothing in the file states, nothing tests, and a future
linker-flag change could silently remove. Given the file's exceptional documentation
standard elsewhere, this is the one invariant left implicit.

**Reproduction.** Not attempted. Verifiable in one command on a release build:
`nm -D target/release-slim/fono | grep vkGetInstanceProcAddr` — no output confirms the
symbol is not dynamically exported and the risk is nil.

**Suggested direction.** Run that check, then record the answer as a comment in the module
docs beside the existing reasoning. If the symbols *are* exported, the fix is to hoist
the `dlopen` and `device_available` out of the `OnceLock` initialiser — resolve the
handle first, probe second, publish third — so no user code runs inside the once-cell.
A `tests/check.sh` assertion on the release artefact would make the property permanent;
the size-budget gate already inspects the built binary's `NEEDED` set, so the machinery
exists.

**Blast radius.** Zero if the check passes (a comment). Otherwise a restructure confined
to `loader()`.

## Unit I — S5 batch

- **`EMULATED_INSTANCE` is an immutable `static` handed out as a mutable pointer**
  (`vk_loader_shim.rs:195`, `:215`): `addr_of!(EMULATED_INSTANCE).cast_mut().cast()`.
  Sound today because nothing writes through it and the comment says so — *"only the
  stubs below ever see this one, so its contents never matter"* — but a future stub that
  wrote through the handle would be UB with no compiler diagnostic. A `static
  EMULATED_INSTANCE: UnsafeCell<u8>` or an `AtomicU8` would make the intent structural
  rather than conventional.
- **`GetWindowTextLengthW` / `GetWindowTextW` is a TOCTOU on the window title**
  (`fono-inject/src/focus.rs:94-99`). If the title grows between the two calls the
  result is silently truncated. This is the documented Win32 idiom and the consequence is
  a shortened title in a context rule, not a fault — recorded for completeness.
- **`QueryFullProcessImageNameW` uses a fixed 1024-code-unit buffer**
  (`focus.rs:142-148`) with no retry on `ERROR_INSUFFICIENT_BUFFER`, so a path longer
  than that yields `None` and the context rule silently loses its `window_class`. The
  degradation is correct; only the absence of a retry is worth noting.

## Unit I — lenses with no findings

- **Lens 2 (error paths), FFI callbacks:** `tap_eval_cb` (`brain_tap.rs:595-688`) is an
  `extern "C"` callback, where a panic would abort the process — and it contains no
  `unwrap`, no `expect`, and no indexing. Every fallible step uses a `let … else`:
  a non-UTF-8 tensor name (`brain_tap.rs:611`), an unparseable tap name
  (`brain_tap.rs:615`), and — notably — a **poisoned capture mutex**
  (`brain_tap.rs:633`), which returns `true` and continues scheduling rather than
  panicking. That is the same `std::sync::Mutex` poisoning question the audit found
  mishandled at four sites in `daemon.rs` and `session.rs` (`F-B005`, `F-C002`,
  `F-A003`), handled correctly here. The one place a poisoning panic would be *fatal* is
  the one place it is guarded.
- **Lens 4 (invariants), layout assumptions:** `BrainTap::install`
  (`brain_tap.rs:405-421`) asserts layout equality between the two `llama_context_params`
  definitions **before** transmuting between them, rather than assuming it. That is the
  correct handling of a duplicated-bindgen hazard.
- **Lens 5 (boundary values), tensor copies:** `copy_tensor_bytes`
  (`brain_tap.rs:552-577`) refuses oversized tensors, tensors with no data, and tensors
  not yet materialised, returning `false` with an empty output rather than reading. Its
  doc comment enumerates all three refusals.

**Worth noting as good code — the best in the project.** `vk_loader_shim.rs` carries 98
lines of module documentation for 350 lines of code, and every paragraph earns its place.
It records *why* returning null is not an option (an indirect call through null before
ggml's guard can react), *why* returning an error is not an option either (Vulkan-Hpp
throws, ggml catches, and on Windows the catch block dies in
`__std_exception_destroy` with `STATUS_HEAP_CORRUPTION` — so the machine that most needs
the CPU fallback is the one that crashes), and *why* the module lives in `fono-core`
rather than in either backend crate (feature unification, and a duplicate-symbol link
error otherwise). It cites the upstream source lines the three bare symbols come from.
The three tests assert exactly the property the design depends on — that the emulation
answers every entry point in ggml's init walk, declines everything else, and reports zero
devices — with the reason in the assertion message: *"zero devices is what keeps ggml on
its exception-free path"*.

## Unit I — summary

| ID | Severity | One line |
|---|---|---|
| `F-I001` | S3 (low conf.) | Vulkan shim re-entrancy safety depends on an unstated non-export property |

**The unit found no memory-safety defect, and that is the finding.** Every `unsafe` block
read in this pass either had a correct `SAFETY` comment or was trivially sound from
context. The pointer discipline is consistent: null checks before every use, handles
closed on every path, out-parameters validated before dereference, and layout assumptions
asserted rather than assumed.

**The real `unsafe` risk in this project is documentation coverage, not correctness**, and
that is already recorded as `F-0004` — 53 of 140 sites carry no safety contract, and 18
of 19 crates declare no `unsafe` policy (`#![forbid(unsafe_code)]` where none is needed,
`#![deny(unsafe_op_in_unsafe_fn)]` where some is). The undocumented sites cluster in the
platform backends rather than in the two hard modules. Adding `#![forbid(unsafe_code)]`
to the crates that contain none is free, permanent, and would shrink the surface a future
reviewer has to consider from 19 files to 7.

---

# Stage 2 — Unit J: local inference backends

**Scope:** `crates/fono-assistant/src/llama_local.rs` (4,280 lines),
`crates/fono-polish/src/llama_local.rs`, `crates/fono-core/src/llama_gen.rs` (the shared
sampler and stop policy), and the prompt-state cache. Plan Task 2.9.

**Method note.** This is the largest single file in the workspace after `session.rs` and
`daemon.rs`, and its comments are unusually evidence-bearing — they cite measured token
counts, observed failure modes and upstream source lines. The audit treated those claims
as testable rather than authoritative, and the unit's main finding is a case where the
code contradicts its own configuration rather than its comments.

## F-J001 — The assistant prefills the whole prompt in one batch while capping `n_batch` at 2048, so long conversations fail to decode

**Severity:** S2 · **Confidence:** high · **Fixed 2026-08-12** — every prefill
site now decodes in `n_batch`-sized chunks, so prompt length is bounded only by
the context. Confirmed by measurement: a 1,232-token prompt through a 512-token
batch, and prefixes up to 9,428 tokens
(`docs/bench/prompt-cache-2026-08-11.md`).

**Location:** `crates/fono-assistant/src/llama_local.rs:1153-1166`,
`crates/fono-assistant/src/llama_local.rs:1634`,
`crates/fono-assistant/src/llama_local.rs:1752`; constants at
`crates/fono-assistant/src/llama_local.rs:135` and
`crates/fono-assistant/src/llama_local.rs:276`; contrast
`crates/fono-polish/src/llama_local.rs:250`

**Observation.** The context is created with a batch size capped at 2,048:

```rust
const DEFAULT_BATCH_SIZE: u32 = 2048;                       // :135
let tuned_batch = DEFAULT_BATCH_SIZE.min(context_size.max(MIN_CTX));   // :276
…
.with_n_batch(batch_size)                                    // :1110
```

The prefill is then built and submitted as **one** batch sized by the *context*, not the
batch:

```rust
let prefill_batch_capacity = self.context_size as usize;     // :1153
let mut batch = LlamaBatch::new(prefill_batch_capacity, 1);
for (i, token) in tokens.iter().enumerate() { batch.add(…)?; }
ctx.decode(&mut batch).context("prefill decode")?;           // :1166
```

The default assistant context is **8192** (`crates/fono-core/src/config.rs:1503`), so the
shipped configuration is `n_batch = 2048` with a prefill batch that may hold up to 8,192
tokens. `llama_decode` rejects a batch larger than `n_batch`. A workspace-wide search for
`chunks(` over the inference crates finds no chunked prefill anywhere — all three assistant
prefill sites (`:1154`, `:1635`, `:1753`) have this shape.

**Why it is wrong.** The only guard on prompt length checks the *context*, not the batch
(`llama_local.rs:1143`):

```rust
if tokens.len() as u32 + (MAX_NEW_TOKENS as u32) >= self.context_size {
```

With `MAX_NEW_TOKENS = 384` and `context_size = 8192`, that admits prompts up to 7,807
tokens — nearly four times what the decode can accept. Everything between 2,049 and
7,807 tokens passes the guard and then fails at `ctx.decode` with the bare context string
`"prefill decode"`, which names no cause and suggests no remedy. The carefully written
error at `:1144-1149` — the one that tells the user to raise `[assistant.local].context`
— fires only in the range where the problem is *not* the batch, so the user gets the
useless message in exactly the case that occurs and the helpful message in the case that
does not.

The user reaches this by ordinary use: the assistant keeps conversation history, and
`config.rs:1501-1502` says the larger context exists *"for short conversation
history"*. Every turn adds tokens, so a conversation that runs long enough crosses 2,048
and the assistant stops answering — permanently, since the history only grows. The system
prompt, the MCP tool schemas and the transcript all count toward it, so agent-style use
with several registered tools reaches it faster.

**The sibling gets it right.** `fono-polish` sets `.with_n_batch(self.context_size)`
(`fono-polish/src/llama_local.rs:250`) and then builds its batch at
`LlamaBatch::new(self.context_size as usize, 1)` (`:279`) — the two agree by
construction, so its single-shot prefill can never exceed the batch. The assistant
introduced a separate `DEFAULT_BATCH_SIZE` for its own reasons and did not adjust the
prefill to match. This is the same divergence shape as `F-F002` and `F-D001`: two copies
of a pattern, one correct.

**Reproduction.** Configure `[assistant.local] context = 8192` (the default) and hold a
conversation until the accumulated prompt exceeds 2,048 tokens — or call the assistant
once with a ~3,000-token prompt. The turn fails with `prefill decode`. The same prompt
succeeds with `[assistant.local].batch` raised to 8192 if the field is exposed, which
confirms the cause.

**Suggested direction.** Chunk the prefill into `n_batch`-sized decodes, which is the
standard llama.cpp idiom and removes the ceiling entirely — positions carry across
batches, so only the final chunk needs `logits = true` on its last token. That also makes
the `LlamaBatch::new(context_size, …)` allocation shrink to `n_batch`, which is a real
memory saving at ctx 8192. The cheap alternative is to size the batch to
`min(batch_size, context_size)` and tighten the guard at `:1143` to compare against
`n_batch` — that turns a confusing failure into a clear one but keeps the 2,048-token
conversation ceiling, so it is a mitigation rather than a fix. Whichever is chosen, all
three prefill sites need it.

**Blast radius.** One prefill helper, called from three sites in `llama_local.rs`. No
config or API change if the chunking route is taken.

## F-J002 — The prompt-state cache key labels a crate version as llama-cpp-2's, so an ABI bump can restore an incompatible blob

**Severity:** S3 · **Confidence:** medium

**Location:** `crates/fono-assistant/src/llama_local.rs:1674-1685`

**Observation.** The cache key's runtime-identity string is:

```rust
let runtime_identity = format!(
    "llama-cpp-2:{}|model={}|size={}|modified={}|ctx={}|threads={}|batch={}|ubatch={}",
    env!("CARGO_PKG_VERSION"),
    …
);
```

`env!("CARGO_PKG_VERSION")` expands to the version of the **crate being compiled** —
`fono-assistant` — not to `llama-cpp-2`. The label asserts one thing and the value is
another.

**Why it is wrong.** The field exists to invalidate cached state when the serialisation
format changes, and that format is owned by llama.cpp. A `llama-cpp-2` bump that changes
the state layout without a `fono-assistant` version bump produces a key collision: the
cache hands back a blob written by the old format and `set_state_data`
(`llama_local.rs:1796`) reads it. The consequence is not a clean error — it is a restored
KV cache that does not correspond to the tokens the key claims, so generation continues
from the wrong state and the user gets a fluent, confident, wrong answer. That is the
`S1` "wrong output reaching the user" shape; it is recorded as S3 only because the
precondition is a dependency bump that in practice accompanies a release.

Two mitigating factors, both incidental rather than designed. Workspace crates share the
root version, so a release bumps this string; and the `Cargo.lock` pin means a
`llama-cpp-2` change is a deliberate act. Neither is a property the code enforces, and
the misleading label is precisely what would stop a reviewer from noticing during such a
bump.

The rest of the key is thorough — model path, file size, mtime, context, threads, batch,
ubatch, prompt hash, token hash and token count — which is what makes the one wrong field
worth recording rather than the key as a whole.

**Reproduction.** `theory-only` — requires a `llama-cpp-2` bump with a state-format
change. Statically verifiable: `env!("CARGO_PKG_VERSION")` in `fono-assistant` is the
`fono-assistant` version, which is trivially confirmed by printing it.

**Suggested direction.** Use the dependency's version, which Cargo exposes to build
scripts, or failing that a hand-maintained `STATE_FORMAT_VERSION` const bumped whenever
`llama-cpp-2` moves — with a comment tying it to the pin. At minimum, relabel the field
so it stops asserting something false; a future reader debugging a stale-state bug will
otherwise rule this out in seconds and be wrong.

**Blast radius.** One format string, plus a const if the explicit-version route is taken.
Changing the string invalidates every existing cache entry once, which is harmless.

## Unit J — lenses with no findings

- **`llama_gen.rs` is the best-argued module in the project**, and it exists because of
  exactly the failure this audit keeps finding. Its module docs record that the polish
  backend fixed both the Gemma verbatim-repetition loop and the dead-stop-token bug in
  2026-05, *"but the assistant kept its own copy of the decode loop and shipped without
  either fix — observed as a refusal sentence repeated to the 384-token cap (~13 s)."*
  The sibling-divergence pattern that produced findings in every unit of this audit was
  already diagnosed here, and the response was to make one definition both backends must
  use. `F-J001` is the same divergence in the part that was **not** unified.
- **Lens 4 (invariants), stop policy:** `is_control_token` stops on the
  `LlamaTokenAttr::Control` attribute rather than on literal marker strings, and the
  comment explains why with specifics — `gemma-4-e2b` spells its turn markers `<|turn>`
  (105) and `<turn|>` (106), so `single_token("<end_of_turn>")` returns `None` and
  `token_to_piece(special = false)` renders them as empty text, making every
  string-based check dead code on that vocab. `warn_on_template_vocab_mismatch` is a
  load-time tripwire for the next model switch. This is a defect class closed rather than
  a defect fixed.
- **Lens 3 (concurrency), sampler acceptance:** the module forces every decode loop
  through `sample_next` because accepting a token the caller already sampled feeds the
  penalty sampler twice, and the comment records that this *"silently disarmed the
  tool-call rails for an entire measurement"*. The hazard is structurally prevented, not
  documented and left to discipline.
- **Lens 5 (boundary values), state save:** `copy_state_data` is checked for both
  `saved_bytes == 0` and `saved_bytes > state_bytes` before the buffer is truncated
  (`llama_local.rs:1784-1789`), so a misbehaving upstream cannot produce a truncate panic
  or a short blob presented as complete.

## Unit J — summary

| ID | Severity | One line |
|---|---|---|
| `F-J001` | S2 | Assistant prefills up to 8,192 tokens into a 2,048 `n_batch`; long conversations fail |
| `F-J002` | S3 | Cache key labels `fono-assistant`'s version as llama-cpp-2's, risking a stale-state restore |

**Both findings sit in the gap `llama_gen.rs` did not close.** That module unified the
sampler and the stop rules across the two backends after a divergence caused a
user-visible 13-second failure. The *batching* was left per-backend, and that is where
`F-J001` lives — the polish backend's batch matches its context, the assistant's does
not. The lesson the file already teaches applies one level further out than it was
applied.

---

# Stage 2 — Unit K: config, paths, persistence

**Scope:** `crates/fono-core/src/config.rs` (3,003 lines — load, migrate, save,
`atomic_write`), `secrets.rs`, `paths.rs`, and the four SQLite stores (`history.rs`,
`conversations.rs`, `api_keys.rs`, `tool_catalog.rs`). Plan Task 2.10. The
read-modify-write race on `config.toml` was already recorded as `F-C006` and is not
repeated here.

**Headline: the on-disk security discipline is uniformly correct, and the schema
discipline is not.**

## F-K001 — 34 of 40 config sections silently discard unknown keys, then erase the evidence on the next save

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono-core/src/config.rs:23` (`Config`) and 33 further structs;
contrast `crates/fono-core/src/config.rs:1334`, `:1363`, `:1831`, `:1868`, `:1928`,
`:1967`

**Observation.** Every deserialised config struct carries `#[serde(default)]`, but only
six also carry `deny_unknown_fields`:

| Guarded | Unguarded |
|---|---|
| `AssistantTools`, `McpClientServer`, `ServerWyoming`, `ServerLlm`, `ServerWeb`, `Network` | `Config`, `General`, `Hotkeys`, `Audio`, `WakeWord`, `Stt`, `Tts`, `Polish`, `Assistant`, `Overlay`, `History`, `Inject`, `Update`, `Server`, `McpServer`, and 19 others |

So `[server.wyoming]` rejects a misspelled key and `[general]` does not.

**Why it is wrong.** A user who writes `langauges = ["en", "ro"]` under `[general]` gets
no error, no warning, and a daemon that starts normally with the default language list.
The setting they wrote has no effect and nothing tells them why.

The second half is what raises this above a routine papercut. `Config::save`
(`config.rs:2303-2311`) serialises the parsed struct, so the next save — a tray
preference toggle, a web-settings change, `fono use`, any of the twenty-odd writers in
`F-C006` — **rewrites `config.toml` without the misspelled line**. The user's typo is
deleted from their file. They cannot find it to correct it, and if they suspect
something was dropped they have no artefact to inspect. A hand-edited config that was
80% applied silently becomes a config that looks like it was never edited.

The divergence is not random: the six guarded structs are the newest sections (servers,
MCP client, tools). The pattern was adopted and not applied retroactively, which is
consistent with every other sibling-divergence finding in this audit.

**Reproduction.** Add `langauges = ["ro"]` to `[general]` in `config.toml` and start the
daemon — it starts clean and uses the default list. Change any preference from the tray,
then reopen `config.toml`: the line is gone. Repeat with a misspelled key under
`[server.wyoming]` and the daemon refuses to start with a message naming the key.

**Suggested direction.** Add `deny_unknown_fields` to the remaining 34 structs, which
makes a typo a startup error naming the offending key — serde's message is already
good. The migration risk is real and needs a decision: any key a previous Fono version
wrote and a current one no longer reads becomes a hard failure, so this wants a pass over
removed fields first, keeping `#[serde(alias)]` or an ignored-and-warned field for each.
A softer variant that avoids that risk entirely is to deserialise once into
`toml::Value`, diff its key set against the parsed struct, and `warn!` per unknown key —
that never blocks startup, still tells the user, and needs no per-field migration work.
Given the silent-deletion behaviour, warning is the minimum; rejecting is better where
the migration cost is understood.

**Blast radius.** One attribute on 34 structs, plus the removed-field survey. The
`toml::Value` diff variant is ~15 lines in `load` and touches nothing else.

## Unit K — S5 batch

- **`atomic_write` does not fsync the containing directory after `persist`**
  (`crates/fono-core/src/config.rs:2336`). The temp file's contents are durable
  (`sync_all` at `:2326`) and the rename is atomic, but on most Linux filesystems the
  *directory entry* created by the rename is not durable until the directory is synced.
  A power loss immediately after a config save can therefore leave the previous config in
  place — the write is atomic but not durable, while the doc comment at `:2302` says only
  *"Atomic write via tempfile + rename"* and is technically accurate. Two lines
  (`File::open(dir)?.sync_all()`) close it. Recorded as S5 because the loss is one
  setting change, never a corrupt file.
- **A config written by a newer Fono cannot be read by an older one, with no recovery
  path** (`config.rs:2257-2262`). `migrate` returns `ConfigVersionTooNew` and `load`
  propagates, so a user who downgrades gets a daemon that will not start. The error names
  the found and supported versions, which is the right information, but suggests no
  remedy. One sentence pointing at the config path would make it actionable.

## Unit K — lenses with no findings

- **On-disk permissions are handled correctly and consistently.** All four SQLite stores
  (`history.rs:311`, `conversations.rs:572`, `api_keys.rs:505`, `tool_catalog.rs:1680`)
  chmod the database **and its `-wal` / `-shm` sidecars** to `0600`, and each has a test
  that creates the file `0644` and asserts it comes back `0600` — the tests verify the
  fix rather than the intention. `secrets.toml` is written `0600` through the same
  `atomic_write` (`secrets.rs:48`). The IPC socket is `0600` (`fono-ipc/src/lib.rs:279`).
  Remembering the WAL sidecars is the part most implementations miss.
- **`atomic_write` sets the mode on the temp file *before* `persist`**
  (`config.rs:2329-2336`), so the file is never visible at the target path with default
  umask permissions. The obvious ordering mistake — persist, then chmod — is absent.
- **Lens 2 (error paths), `Config::load`:** correct discrimination. A missing file
  yields defaults (`config.rs:2251`); a *malformed* file returns `TomlParse` with the path
  and propagates. The tempting shortcut — falling back to defaults on any error — would
  silently discard a user's entire configuration after one bad edit, and is not taken.
- **No secret is logged anywhere.** A sweep of every `info!`/`warn!`/`error!`/`debug!`/
  `println!` containing `api_key`, `secret`, `password` or `bearer` across all 272 source
  files returns only key *names* and file *paths*. The one place a secret is printed is
  `fono keys create` (`crates/fono/src/cli.rs:2478`), which is the deliberate
  show-once path and says so: *"This secret is shown only once — copy it now. It is
  stored only as a hash and can never be shown again."*
- **Lens 4 (invariants), prompt migration:** `migrate` refreshes baked-in prompts by
  matching against a list of superseded literals rather than by version gate
  (`config.rs:2268-2280`), with the reasoning stated — a genuine customisation never
  matches a superseded literal, so it survives. Same technique for the local polish model,
  which additionally requires quantization and context to still match the default shape
  before touching the model name. Both are the careful version of a migration that is
  usually written to clobber.

## Unit K — summary

| ID | Severity | One line |
|---|---|---|
| `F-K001` | S3 | 34 of 40 config sections ignore unknown keys, then delete them on the next save |

**This unit is the inverse of Unit H.** There, a security control was documented and
absent; here every security-relevant behaviour — file modes, WAL sidecars, temp-file
ordering, secret redaction, malformed-input handling — is correct, tested, and consistent
across four independent stores. The single finding is a usability failure, not a safety
one.

**`F-K001` compounds with `F-C006`.** That finding records twenty-odd unsynchronised
writers of `config.toml`; this one records that every such write silently strips content
the parser did not recognise. Together they mean a hand-edited config is at risk from two
directions — a concurrent writer reverting it, and any writer erasing the parts that were
mistyped.

---

# Stage 2 — Unit L: cloud provider backends

**Scope:** the cloud STT, TTS, polish and assistant-chat backends across `fono-stt`,
`fono-tts`, `fono-polish` and `fono-assistant` (~22,700 lines), plus the shared
`fono-http` body/SSE watchdog layer. Plan Task 2.11, which asks that this unit be audited
*primarily for sibling divergence* — sixteen providers implementing one interface.

**Method note.** Rather than read sixteen near-identical files, every provider was
measured against the same five questions mechanically: where does its HTTP client come
from, what timeouts does that client carry, does it use the shared body watchdog, does it
capture a provider request id, and does it emit the debug trace. The divergences those
tables exposed are the findings.

## F-L001 — Cloud transcription is capped at 45 seconds total while recording length is unbounded, so a long dictation is lost

**Severity:** S2 · **Confidence:** high

**Location:** `crates/fono-stt/src/groq.rs:164`; contrast
`crates/fono-stt/src/wyoming.rs:42` and `crates/fono/src/session.rs:62`

**Observation.** Every cloud STT provider — `openai.rs:127`, `deepgram.rs`,
`elevenlabs.rs`, `cartesia.rs`, `gemini.rs`, `groq_streaming.rs`, `openrouter.rs` —
constructs its client from one helper in `fono-stt`:

```rust
reqwest::Client::builder()
    …
    .timeout(std::time::Duration::from_secs(45))     // groq.rs:164
    .connect_timeout(std::time::Duration::from_secs(5))
```

`reqwest`'s `.timeout()` is the deadline for the **entire** request/response, upload and
response body included — not a connect or idle timeout.

Against that, there is no upper bound on how long a user may record. `session.rs:62`
defines `MIN_RECORDING = 300 ms` and a search for a maximum finds none: the capture runs
until the hotkey is released. The project's own Wyoming STT backend sets
`TRANSCRIBE_TIMEOUT = 300 s` (`fono-stt/src/wyoming.rs:42`), which is the same team's
estimate of how long transcription can legitimately take — nearly seven times the cloud
budget.

**Why it is wrong.** The 45 seconds must cover uploading the audio and the provider
transcribing it. A two-minute dictation is roughly 3.8 MB of 16 kHz mono PCM before
encoding; on a domestic uplink the upload alone can consume most of the budget, and
provider-side latency scales with audio length on top of that. When the deadline expires
the request is aborted, and there is no retry anywhere in the pipeline — a search for
`retry` in `session.rs` returns nothing. **The recording is gone.** The user held a
hotkey for two minutes, spoke, and receives an error instead of text, with no way to
recover the audio.

The failure is also worst exactly where it is least expected: short dictations always
succeed, so the ceiling is invisible until a user does something long — reading a
paragraph, dictating a commit message, narrating a bug report — which is when losing the
input costs the most.

The number is not wrong for the workload it was chosen for. It is applied to a workload
whose duration the user controls and the code does not bound.

**Reproduction.** Configure any cloud STT backend, hold the dictation hotkey for two to
three minutes while speaking, and release. The request fails on the reqwest timeout and
no transcript is produced. The same audio through `[stt] backend = "wyoming"` succeeds,
because that path allows 300 s.

**Suggested direction.** Scale the deadline to the audio rather than fixing it — a base
allowance plus a per-second term computed from the PCM length, applied per request with
`RequestBuilder::timeout` instead of on the client, which `reqwest` supports directly.
That keeps short dictations failing fast, which is the property the 45 s was protecting.
Worth pairing with a bound on recording length, or at minimum a warning when a capture
passes the point where the configured backend can still deliver — silently accepting audio
the pipeline cannot process is the underlying problem, and the timeout is only where it
surfaces. If neither is done, raising the constant to match `wyoming.rs`'s 300 s trades a
lost dictation for a slow failure, which is strictly better.

**Blast radius.** One helper plus a length parameter threaded to the request builder; no
API change if the per-request form is used.

## F-L002 — Four functions named `warm_client`, four different timeout policies, one of them public

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono-tts/src/openai_compat.rs:205` (public),
`crates/fono-stt/src/groq.rs:158`, `crates/fono-polish/src/openai_compat.rs:105`,
`crates/fono-assistant/src/openai_compat_chat.rs:163`

**Observation.** Each of the four provider crates defines its own `warm_client`, and the
four disagree on every axis that matters:

| Crate | Location | Connection pool | Protocol | Overall timeout |
|---|---|---|---|---|
| `fono-tts` | `openai_compat.rs:205` (**`pub`**) | **disabled** (`pool_max_idle_per_host(0)`) | **HTTP/1.1 only** | 30 s |
| `fono-stt` | `groq.rs:158` | 4 per host | HTTP/2 + keepalive | 45 s |
| `fono-polish` | `openai_compat.rs:105` | 4 per host | HTTP/2 + keepalive | 30 s |
| `fono-assistant` | `openai_compat_chat.rs:163` | 4 per host | HTTP/2 + keepalive | **none** |

Sixteen provider implementations select among these four by which crate they happen to
live in, not by what their workload needs.

**Why it is wrong.** The per-crate differences are individually justified — the assistant
omits an overall timeout because it streams and relies on `SSE_CHUNK_TIMEOUT`
(`openai_compat_chat.rs:66`) instead, which is correct; and the TTS variant's disabled
pool and forced HTTP/1.1 carry an excellent comment recording the OpenRouter
`/audio/speech` proxy hang that motivated them. The problem is that four materially
different objects share one name, and the reasoning for each lives only beside its own
definition.

Two concrete consequences. A reader in `fono-stt` who greps `warm_client` finds four
definitions and cannot tell from a call site which applies; the TTS one is `pub`, so an
`use` from another crate would compile and silently impose a no-pooling, HTTP/1.1,
30-second client on a workload none of that was chosen for. And the divergence in the
*overall timeout* column is the substantive one — it is the axis `F-L001` turns on, and
nothing in the code marks 45 versus 30 versus none as a deliberate contrast rather than
drift.

**Reproduction.** Not applicable — statically evident from the four definitions.

**Suggested direction.** One builder in `fono-http`, which already owns the shared
body/SSE layer, taking an explicit workload descriptor — pooling, protocol and deadline —
so each call site names what it needs and the reasoning sits with the parameter rather
than with a copy. The four current behaviours all survive as arguments; nothing needs to
change semantically. Failing that, rename each to say what it is
(`tts_no_pool_client`, `stt_client`, `chat_streaming_client`) and make the TTS one
private, which costs nothing and removes the collision.

**Blast radius.** One new helper plus sixteen call sites, all mechanical. `fono-http` is
already a dependency of all four crates, so **net-zero on binary size**.

## Unit L — S5 batch

- **All four `warm_client` builders end in `.unwrap_or_default()`.** If `Client::builder`
  fails, the fallback is `reqwest::Client::default()` — which has **no timeouts, no
  keepalive and default pooling**, i.e. precisely the configuration the builder was
  called to avoid. The failure is silent. `.expect("build http client")` would be
  defensible here since a failure indicates a broken TLS backend rather than a runtime
  condition; at minimum it warrants a `warn!`.
- **`crates/fono-tts/src/discovery.rs:51` uses `reqwest::Client::new()`** — no connect
  timeout and no overall timeout, the only provider-adjacent client in the workspace with
  neither. The module defines `DEFAULT_DISCOVERY_TIMEOUT = 10 s` (`discovery.rs:22`) but
  it is not applied to the client, so a voice-discovery request to an unresponsive host
  hangs on the operating system's TCP timeout.
- **`crates/fono-assistant/src/mcp_client.rs:108` builds a client with neither an overall
  timeout nor a `connect_timeout`.** The comment justifies omitting the overall timeout —
  *"The SSE stream idles between our requests; no total-response timeout, only the outer
  deadline"* — which is right, but says nothing about connect. Every other client in the
  workspace sets `connect_timeout(5s)`, so an unreachable MCP endpoint blocks for the OS
  default rather than five seconds.

## Unit L — lenses with no findings

- **`fono-http` is a genuinely good abstraction, correctly reasoned.** Its module docs
  state the problem precisely: `reqwest`'s timeout is a *total* deadline, so a body that
  stalls forever is only caught after the whole budget elapses, and a slow-but-progressing
  connection is killed even though it is working. `read_body_with_watchdog`
  (`body.rs:160-168`) replaces both behaviours with an inter-chunk deadline. That is the
  right shape, and it is the mechanism `F-L001` should be extended with rather than
  replaced.
- **`BodyError::is_retryable` (`body.rs:80-91`) discriminates correctly** — connect,
  timeout and request errors are retryable; decode errors are not, with the reason stated:
  *"malformed bytes — retrying just risks the same."* The distinction most
  implementations skip is present.
- **Lens 2 (error paths), the OpenAI-compatible family:** the four backends that carry
  the full observability set — `provider_request_id` plus `emit_http_debug` plus the
  watchdog — are `fono-stt/openrouter.rs`, `fono-tts/openai_compat.rs`,
  `fono-polish/openai_compat.rs` and `fono-assistant/openai_compat_chat.rs`. These are
  the paths a support question is most likely to reach, and they are the ones that can
  answer it.

**A note on what this unit did *not* find.** The expected defect in a sixteen-provider
surface is inconsistent error mapping — one provider reporting a 401 as a network fault,
another swallowing a 429. That was checked and is not present; the classification is
centralised. The divergence in this codebase is in *transport configuration*, one layer
below where it was looked for.

## Unit L — summary

| ID | Severity | One line |
|---|---|---|
| `F-L001` | S2 | Cloud STT has a 45 s total deadline; recording length is unbounded and the audio is lost |
| `F-L002` | S3 | Four `warm_client` functions, four timeout policies, one public and importable |

**The two findings are one measurement read at two scales.** `F-L002` records that the
transport deadline was decided four times independently; `F-L001` records what happens in
the one case where the number chosen does not match the workload it governs. Consolidating
the client — the `F-L002` suggestion — is what makes `F-L001` a one-line change instead
of a decision to re-take.

---

# Stage 2 — Unit M: injection and platform integration

**Scope:** `crates/fono-inject/` (2,948 lines) — backend selection and dispatch
(`inject.rs`), the focus-detection cascade (`focus.rs`), the window classifier
(`classifier.rs`), the `/proc` terminal enrichment (`terminal.rs`), and the XTEST typing
backend (`xtest_type.rs`). Plan Task 2.12.

**Why this unit matters.** It is the last hop. Everything upstream — capture, STT,
polish — is wasted if the text does not land, and this is the only unit where a failure
destroys work the user cannot regenerate.

## F-M001 — Three of five injection backends report success when they failed, so the dictation is lost and the user is told it worked

**Severity:** S1 · **Confidence:** high

**Location:** `crates/fono-inject/src/inject.rs:575-585`; consumed at
`crates/fono-inject/src/inject.rs:239-250` and `crates/fono/src/session.rs:593-601`

**Observation.** All three subprocess backends — `wtype`, `ydotool`, `xdotool` — dispatch
through one helper:

```rust
fn inject_subprocess(cmd: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(cmd) … .status()
        .map_err(|e| anyhow!("spawning {cmd} failed: {e}"))?;
    if !status.success() {
        warn!("{cmd} exited with {status}");
    }
    Ok(())
}
```

A non-zero exit is logged and then **discarded**. The function returns `Ok(())` for every
outcome except a failure to *spawn*.

**Why it is wrong.** The clipboard fallback — the mechanism the module exists to
guarantee — is keyed entirely on that return value (`inject.rs:239-250`):

```rust
match inj.inject(text) {
    Ok(()) => Ok(InjectOutcome::Typed(…)),
    Err(key_err) => match copy_to_clipboard(text) { … }
}
```

So a tool that starts and then fails is indistinguishable from one that typed the text.
`type_text_with_outcome` returns `Typed("ydotool")`, `RealInjector::inject`
(`session.rs:594-601`) logs a successful backend, the orchestrator marks the dictation
delivered, and **the clipboard is never populated**. The text exists nowhere. The audio
has already been discarded.

This is not a hypothetical exit code. Each of the three fails this way in ordinary
conditions: `ydotool` exits non-zero when `ydotoold` is not running or the user lacks
access to `/dev/uinput`, which is the single most common ydotool complaint; `xdotool`
exits non-zero when `DISPLAY` points at a server it cannot reach; `wtype` exits non-zero
on a compositor lacking the virtual-keyboard protocol. The `wtype` case is doubly
instructive — `inject.rs:122-127` documents that Mutter makes `wtype` *"exit 0 silently
while typing nothing"* and adds a registry probe to avoid selecting it. The author
identified the exact class of failure, mitigated the sub-case where the exit code lies,
and left the case where the exit code tells the truth unhandled.

The two remaining backends do this correctly. `inject_enigo` (`inject.rs:588-594`)
propagates both init and text errors, and `type_via_xtest` (`xtest_type.rs:59-80`)
returns `Err` with context on every failure. So the fallback works for two of five
backends and silently does not for three — and the three are the defaults on most Linux
sessions (`detect_auto_linux_desktop` reaches `Wtype`, `Ydotool` or `Xdotool` before
either of the working ones on any Wayland session, `inject.rs:128-139`).

The doc comment at `inject.rs:232-235` states the guarantee this breaks: *"Always
succeeds if at least one of {key-injection, clipboard tool} works on the host … so fono
can never silently lose a dictation."* Silently losing a dictation is exactly what
happens.

**Reproduction.** On a Wayland session with `ydotool` installed and `ydotoold` **not**
running, dictate anything. The tray reports success, the log shows
`ydotool exited with exit status: 1` at `warn!`, no text is typed, and the clipboard is
unchanged. Equivalent with `xdotool` and `DISPLAY=:99` pointing at nothing.

**Suggested direction.** Return `Err` on a non-zero exit so the existing fallback fires —
the three-line change is `if !status.success() { return Err(anyhow!("{cmd} exited with
{status}")); }`. Worth capturing the tool's stderr into that error, since
`copy_to_clipboard_all` already demonstrates the pattern (`inject.rs:496`) and the
message is what a user will paste into a bug report. Note this makes the fallback fire in
cases that currently appear to work, which is the point — but it will also newly surface
tools that exit non-zero *after* typing successfully, if any do; a short survey of the
three tools' exit semantics should precede the change.

**Blast radius.** Three lines in one helper. No API change; `type_text_with_outcome`
already handles the `Err` path.

## F-M002 — A misspelled `[inject].backend` silently disables typing altogether

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono-inject/src/inject.rs:53`

**Observation.** The forced-backend override ends with a catch-all:

```rust
return match forced.to_ascii_lowercase().as_str() {
    "xdotool" => Self::Xdotool,
    …
    "clipboard" | "none" => Self::None,
    "auto" => Self::detect_auto(),
    _ => Self::None,                      // inject.rs:53
};
```

An unrecognised value produces `Self::None`, which is the same value `"clipboard"`
produces — the caller cannot distinguish "the user asked for clipboard" from "the user
asked for something that does not exist".

**Why it is wrong.** `inject.rs:36-37` records that this variable is *"the channel through
which fono's config (`[inject].backend`) propagates to this crate"*, so a typo in
`config.toml` lands here. `xdootol`, `wtype ` with a trailing space, or `XTest` on a build
without the `x11-paste` feature all resolve to `None`. The user has configured a typing
backend and gets clipboard-only operation with no error at startup, no warning in the log,
and a tray that reports normal function. The symptom — "my dictations stopped typing and
now I have to press Ctrl+V" — gives no hint of the cause.

The feature-gated arms make this worse than a plain typo case: `"enigo"` and `"xtest"` are
valid spellings that *silently* fall through to `None` on a build where the feature is
off, so the same config behaves differently across builds of the same version with no
diagnostic.

This compounds with `F-K001`, which records that 34 of 40 config sections silently ignore
unknown *keys*. Here the key is recognised and the *value* is silently ignored, so the
two findings cover the two halves of the same user-facing failure.

**Reproduction.** Set `[inject] backend = "xdotoool"` and restart. Dictation goes to the
clipboard; nothing reports why.

**Suggested direction.** Warn on the unrecognised branch, naming the value and the valid
set — the information is all present at that point and one `warn!` makes the failure
self-diagnosing. Better, validate at config load rather than at first injection, so the
error appears at startup next to the setting that caused it; `fono use inject` already
constrains the value, so this only affects hand-edited configs. Feature-gated names should
warn distinctly (*"this build has no enigo backend"*) rather than sharing the typo
message.

**Blast radius.** One branch, or a validation function in the config loader.

## Unit M — S5 batch

- **`Injector::detect()` runs the full cascade on every injection**
  (`inject.rs:237`). On a Wayland session that means an env read, a `which` walk of
  `PATH` for up to three tools, and a Wayland registry round-trip
  (`compositor_supports_virtual_keyboard`) per dictation. The result cannot change within
  a session in practice. `warm_backend` (`inject.rs:523`) already runs the same detection
  at startup and throws the answer away. Caching it in a `OnceLock` would remove the
  per-dictation cost and — more usefully — make the selected backend a single observable
  value the tray and `fono doctor` could report.
- **`sway_focus` allocates a compositor-declared length before reading**
  (`focus.rs:334-338`): `payload_len` comes off the socket as a `u32` and sizes a
  `vec![0u8; payload_len]` directly, with no ceiling. This is the fourth instance of the
  `F-G001`/`F-G002`/`F-G003` pattern. It is S5 rather than S3 only because the peer is
  the user's own compositor over a `$SWAYSOCK` Unix socket, so a hostile length implies
  an already-compromised session. Worth a cap for consistency if that family is fixed
  together.
- **`QueryFullProcessImageNameW` truncation** and the **`GetWindowTextLengthW` TOCTOU**
  were recorded in Unit I (`F-I001`'s S5 batch) and are not repeated here.

## Unit M — lenses with no findings

- **The `Private` profile is correct and correctly placed.** `private_profile`
  (`classifier.rs:237-247`) sets `suppress_history: true` and no LLM suffix, and the rule
  covers `keepassxc`, `bitwarden`, `1password`, `gnome-keyring` and `seahorse` plus their
  Windows executable names (`classifier.rs:460-475`). Dictating into a password manager
  does not reach the history database and is not sent to a polish provider. This is the
  one place in the product where a classification miss would be a privacy incident, and
  it is handled.
- **Lens 2 (error paths), the focus cascade:** every probe in
  `detect_focus_linux_desktop` (`focus.rs:203-294`) logs its failure at `debug!` and
  falls through to the next, ending at an empty `FocusInfo` rather than an error — the
  right shape for a best-effort enrichment. The GNOME 46+ `AccessDenied` case is
  discriminated from other D-Bus errors and gets its own explanatory log line
  (`focus.rs:259-263`).
- **Lens 8 (sibling divergence), platform probes:** the three platform bodies are
  correctly gated on `target_os` rather than on `not(linux)`, with the reasoning stated —
  `focus.rs:197-201` explains that the cascade probes *"Linux display-server environments
  that carry no signal elsewhere"*, and `inject.rs:98-100` says the same for injection.
  The usual defect here is a `#[cfg(unix)]` that accidentally covers macOS; it is absent.

**Worth noting as good code.** `copy_to_clipboard_native` (`inject.rs:337-382`) carries a
25-line comment that is the best failure post-mortem in the codebase: a fresh
`arboard::Clipboard` per call released the X `CLIPBOARD` selection within microseconds of
returning, so hosts *without* an ICCCM clipboard manager — i3/sway/dwm with no
clipit/parcellite/klipper — **lost the dictation entirely**, while hosts *with* one got a
`SAVE_TARGETS` timeout warning and a racy cached copy. The fix holds one handle for the
process lifetime and recreates it on `set_text` failure. The comment records the symptom,
the mechanism, the two host populations that saw different faults, and why the chosen fix
covers both.

## Unit M — summary

| ID | Severity | One line |
|---|---|---|
| `F-M001` | S1 | `wtype`/`ydotool`/`xdotool` report success on failure; the dictation is lost silently |
| `F-M002` | S3 | An unrecognised `[inject].backend` value silently degrades to clipboard-only |

**`F-M001` is the most consequential finding in the audit so far.** It is the only one
where the product destroys user work and reports success while doing it, it is reachable
through the default backend selection on most Linux Wayland sessions, it fires on the
single most common `ydotool` misconfiguration, and the fix is three lines. The
surrounding module is otherwise unusually careful about exactly this failure mode — the
`wtype`-exits-0 probe and the `arboard` handle-lifetime fix are both defences against
losing a dictation — which is what makes the gap in the shared dispatch helper worth
raising above every other finding recorded.

**Both findings are silent substitutions**, the pattern first recorded in `F-B005` and
now seen in seven units: a failure is converted into a value that means something else —
here, `Ok(())` meaning "typed" and `Self::None` meaning "the user chose clipboard".

---

# Stage 2 — Unit N: rendering and overlay

**Scope:** `crates/fono-overlay/` (8,932 lines) — backend selection and the handle
(`backend.rs`), the five surface backends (`backends/`), the software renderer
(`renderer.rs`, 2,729 lines) and the Glass Cortex replay renderer (`cortex.rs`), plus the
`fono-tray` command channel. Plan Task 2.13.

**Headline: the renderer is clean and the transport is not.** Every pixel write in 2,729
lines of software rasteriser goes through a bounds-checked accessor; every command sent to
the backend is dropped without a word if the backend is gone.

## F-N001 — If an overlay backend thread ends, the handle keeps accepting commands forever and nothing notices

**Severity:** S3 · **Confidence:** high

**Location:** `crates/fono-overlay/src/backend.rs:242-245`; thread-exit paths at
`crates/fono-overlay/src/backends/winit_x11.rs:229-231` and
`crates/fono-overlay/src/backends/winit_x11.rs:67-69`

**Observation.** Every public method on `OverlayHandle` — `set_state`, `update_text`,
`push_level`, `push_samples`, `push_fft_bins`, `set_volume_bar`, `push_gate_metrics`,
`set_waveform_style`, `push_cortex` — funnels through one private helper:

```rust
fn send(&self, cmd: OverlayCmd) {
    let _ = self.inner.tx.send(cmd);
    (self.inner.waker)();
}
```

`std::sync::mpsc::Sender::send` returns `Err(SendError)` when the receiver has been
dropped, which happens as soon as the backend thread returns. That result is discarded.
The handle exposes no health accessor — `backend_id()` and `backend_capabilities()`
report the values captured at spawn and never change.

**Why it is wrong.** The backend thread has ordinary exit paths that are not shutdown.
`window_event` calls `el.exit()` on `WindowEvent::CloseRequested`
(`winit_x11.rs:229-231`), which ends `run_app`, returns from `run_event_loop`, and drops
the receiver. A panic anywhere in the 2,729-line renderer does the same — the spawn
closure at `winit_x11.rs:67-69` logs only the `Err` return, so a panic unwinds past it
with no handler at all. In both cases the thread is gone and the daemon has no way to
learn it.

From that moment the overlay is permanently dead and permanently invisible as a fault.
Every subsequent `set_state` returns cleanly; the orchestrator's 22 `set_state` call sites
in `session.rs` all succeed; `fono doctor` still reports `x11-override-redirect` as the
live backend, because `backend_id()` is a stored constant. The user sees no on-screen
indicator for the rest of the session and there is nothing in the log at any verbosity to
connect it to the event that killed the thread.

The contrast with startup is what makes this a defect rather than a policy. When the
overlay fails to spawn, the code goes to real trouble to tell the user: `session.rs:1029`
detects the noop backend landing in a graphical session and emits both a `warn!` and a
desktop notification naming the likely missing package and the command that installs it.
The same outcome — no overlay in a graphical session — is reported meticulously if it
happens at second zero and not at all if it happens at second one.

This is the silent-substitution pattern recorded in seven prior units, in its purest
form: `let _ =` converts "the overlay is gone" into "sent successfully". `fono-tray` has
the identical shape at `crates/fono-tray/src/lib.rs:373`.

**Reproduction.** `theory-only` for the natural case. Deterministic demonstration: panic
inside a renderer draw call, then continue dictating. The X11 thread dies, every
`set_state` still returns, and the tray, the log and `fono doctor` all continue to report
a healthy overlay.

**Suggested direction.** Have `send` observe the error, latch a `dead` flag on
`HandleInner`, and log once at `warn!` on the transition — one line of new state, and the
"once" matters because the alternative is a log line per audio frame. That flag is then
worth surfacing where the startup path already surfaces its equivalent: `fono doctor` and
the same notification text. Restarting the backend is a larger question and probably not
worth it (winit forbids a second `EventLoop` per process, which `session.rs:997-1003`
documents), so telling the user is the achievable goal. The same treatment applies to
`fono-tray/src/lib.rs:373`.

**Blast radius.** One `AtomicBool` in `HandleInner`, one branch in `send`, one accessor.
No API break.

## Unit N — S5 batch

- **The daemon never calls `OverlayHandle::shutdown()`.** The only two call sites are in
  `crates/fono/src/cli.rs:3242` and `:3306`; `daemon.rs` and `session.rs` have none, and
  `HandleInner` implements no `Drop`. The overlay thread is therefore reaped by process
  exit rather than told to stop, on every exit path including the clean Ctrl-C one. Benign
  today — process teardown closes the surface — but it means the graceful path
  `shutdown()` implements (send `Shutdown`, join the thread) is exercised only by the CLI
  demo commands. If the `F-B001` / `F-B002` shutdown channel lands, this is the natural
  place to call it from.
- **The command channel is unbounded** (`std::sync::mpsc::channel()` in every backend).
  `push_samples` and `push_fft_bins` carry a `Vec<f32>` per audio frame, so a backend
  thread that stalls — a compositor freeze, a slow `UpdateLayeredWindow` — accumulates
  frames without limit. The X11 backend drains fully with `try_recv` on every
  `about_to_wait` (`winit_x11.rs:241`) so it recovers, and the audio path is the only
  high-rate producer, which keeps this at S5. A `sync_channel` with a small bound, or
  dropping the oldest frame, would make the backpressure explicit — for visualisation data
  the correct policy is to discard, not to queue.

## Unit N — lenses with no findings

- **The software renderer is exemplary on memory safety.** Across 2,729 lines with
  hundreds of computed pixel indices, **every** framebuffer write goes through
  `buf.get_mut(idx)` and an `if let Some(slot)` — there is not one direct `buf[idx]`
  store. Negative coordinates are filtered before the `as u32` cast that would otherwise
  wrap them into the previous scanline (`renderer.rs:351`, `:402-408`), which is the
  classic defect in this shape and is absent. The one direct slice index in the file,
  `frame[idx_lo..=idx_hi]` at `renderer.rs:831`, was traced: `pos_lo <= pos_hi` holds by
  construction, both are clamped to `[0, bins_len-1]`, and the function returns early on
  an empty frame — so the range cannot invert or exceed.
- **Lens 5 (boundary values), the 3-D paths:** `draw_terrain`
  (`renderer.rs:1068-1102`) handles the empty-history case with a synthetic idle ripple
  rather than an early return, so there is no divide-by-zero and no index into an empty
  `frames`. Every ratio denominator in the file uses `.max(2) - 1` or `.max(1.0)`.
  `heatmap_render_column` guards all three of empty frame, zero width and zero rows
  (`renderer.rs:816`).
- **Lens 2 (error paths), backend selection:** `spawn_overlay`
  (`backend.rs:423-458`) cannot fail in practice — the candidate walk ends at `Noop`,
  which is a terminal sink that always succeeds — and the unreachable tail returns an
  error rather than unwrapping. An unrecognised `FONO_OVERLAY_BACKEND` value **warns and
  falls back to auto-detection** (`backend.rs:426-432`), which is exactly the handling
  `F-M002` found missing for `[inject].backend` and `F-K001` found missing for unknown
  config keys. Same class of input, correct treatment, in a third file.

**Worth noting as good code.** `backend.rs`'s module docs open by recording a **deviation
from the design plan and why** — the unified `pump` + `with_framebuffer` trait was
abandoned because winit owns its event loop and the Wayland backends own theirs, and
*"forcing all three into a common `pump` shape costs more than it gains (the pure renderer
split is already where 95 % of the leverage is)"*. It then lists what was kept. A design
that diverged from its plan deliberately, with the reasoning preserved at the point of
divergence, is rare.

## Unit N — summary

| ID | Severity | One line |
|---|---|---|
| `F-N001` | S3 | A dead overlay backend thread is undetectable; every later command is silently dropped |

**The unit is a clean split between layers.** The renderer — by far the larger and more
intricate half, and the one where a defect would be a crash — is the most consistently
defensive code in the workspace. The transport above it discards the one error that says
the whole subsystem has stopped working.

**`F-N001` completes the silent-substitution set.** The pattern now has instances in eight
units: a poisoned mutex reading as "no MCP activity" (`F-B005`), a panicked drain reading
as "recording too short" (`F-A003`), a non-zero exit reading as "typed successfully"
(`F-M001`), an unknown backend name reading as "the user chose clipboard" (`F-M002`), and
here a dead channel reading as "delivered". Every one is a discarded `Err` or a
neutral-looking default standing in for a specific false claim. This is the strongest
candidate for a single cross-cutting recommendation in the Task 2.15 synthesis.

---

# Stage 2 — Unit O: update, download, bench, CLI

**Scope:** `crates/fono-update/` (1,003 lines — release discovery, download, verify,
atomic swap), `crates/fono-download/` (163 lines — the shared model fetcher with
Range-resume), and the `fono update` / `fono doctor` CLI surface in
`crates/fono/src/cli.rs`. Plan Task 2.14.

**Why this unit matters.** It is the only code in the product that **replaces the running
binary** and the only code that writes attacker-influenceable bytes to disk as executable
model data. A defect here is not recoverable by restarting.

## F-O001 — A failed `persist` after the backup rename leaves the user with no `fono` binary

**Severity:** S2 · **Confidence:** high

**Location:** `crates/fono-update/src/lib.rs:640-654`

**Observation.** The swap is two renames with nothing between them that can undo the
first:

```rust
let backup = target.with_extension("bak");
let _ = std::fs::remove_file(&backup);
if let Err(e) = std::fs::rename(&target, &backup) {   // :642  — target now GONE
    anyhow::bail!("cannot rename {} -> {} ({e}); {}", …);
}
tmp.persist(&target)                                   // :654  — may fail
    .map_err(|e| anyhow!("persist into {}: {}", target.display(), e.error))?;
```

If `persist` fails, the function returns `Err` and **nothing renames `backup` back to
`target`**. There is no `Drop` guard, no `match` with a recovery arm, and no retry.

**Why it is wrong.** After the first rename the installed binary no longer exists at its
path. The user's `fono` command is gone, their systemd unit's `ExecStart` points at a
missing file, and their desktop autostart entry fails silently at next login. What is left
is a `fono.bak` next to where `fono` used to be — recoverable only by a user who knows to
look, and only if they still have a shell command to look with.

The error message compounds it: `persist into /usr/local/bin/fono: …` describes the step
that failed and says nothing about the state the machine is now in or how to get back. A
user reading it has no reason to suspect their binary was moved.

The rest of this function is written to fail safe, which is what makes the gap
conspicuous. The directory is probed for writability **before** downloading
(`lib.rs:562-565`) precisely so a permission problem surfaces early; the download goes to
a sibling temp file so an interrupted transfer cannot touch the target; the SHA mismatch
path bails with *"refusing to apply (running binary unchanged)"* (`lib.rs:608-614`); the
`.bak` exists specifically so the caller can roll back. Every hazard on the path was
anticipated except the one where the rollback artefact is created and then stranded.

`persist` failing is not exotic. It is `rename(2)` plus tempfile's checks: on Windows the
destination directory may be locked by an indexer or an anti-virus scanner mid-update; on
Linux a restrictive SELinux or AppArmor policy can permit creating a dot-prefixed temp
file and deny renaming to the binary name; and `remove_file(&backup)` on the line above
discards its own error, so a `.bak` that could not be removed is a signal already thrown
away.

**Reproduction.** Make `persist` fail deterministically — on Linux, run `fono update`
under a policy that denies `rename` to the target name, or on Windows hold an exclusive
handle on the target directory. Observe that `fono` no longer exists and `fono.bak` does.

**Suggested direction.** Restore on failure: match on `persist`, and in the error arm
rename `backup` back to `target` before returning, so the machine ends in the state it
started in. The error message should say which of the two outcomes occurred — restored,
or restored-failed-too and here is the path to your backup. A `Drop`-guard type holding
`(backup, target)` and disarmed after a successful `persist` is the version that also
covers a panic between the two renames.

**Blast radius.** One `match` in `apply_update`, or one small guard struct. No API change.

## F-O002 — A server that ignores `Range` produces a corrupt file, and for unpinned assets it is renamed into place

**Severity:** S2 · **Confidence:** high

**Location:** `crates/fono-download/src/lib.rs:106-142`, specifically `:113` and `:131`;
unpinned default at `crates/fono-audio/src/wake_registry.rs:190`

**Observation.** Resume is requested by header and the response is accepted without
checking whether the server honoured it:

```rust
if existing > 0 { builder = builder.header("Range", format!("bytes={existing}-")); }
let resp = builder.send().await?;
let status = resp.status();
if !status.is_success() && status.as_u16() != 206 { return Err(…); }   // :113
…
let mut file = OpenOptions::new().create(true).append(existing > 0).write(true)
    .open(dest).await?;                                                 // :131
```

`is_success()` covers **200**. A server that ignores `Range` — a CDN edge, a caching
proxy, a mirror without byte-range support — answers `200 OK` with the **whole** body, and
the code appends that whole body after the `existing` bytes already on disk. The result
is a `.part` file of `existing + full_size` bytes whose first `existing` bytes are a
duplicated prefix.

**Why it is wrong.** For a **pinned** asset the SHA check catches it: the mismatch branch
at `lib.rs:61-68` deletes the `.part` and restarts, with the comment *"Complete but
corrupt: resuming can't repair it, so start over"* — so the outcome is a wasted download,
twice, before it succeeds. That is the cost, not the defect.

For an **unpinned** asset there is no check at all. `lib.rs:55-57` short-circuits:

```rust
if !pinned { info!("downloaded {dest:?}: sha256={actual} (unpinned)"); break; }
```

and the corrupt file is renamed to its final path (`lib.rs:87`). Callers gate on
`dest.exists()`, so the bad file is trusted from then on — permanently, since nothing
re-verifies a file that is already present. The user's symptom is a model that fails to
load with an ONNX parse error, or worse, one that loads and behaves oddly, with no
indication that the file on disk is wrong and no mechanism that would ever replace it.

This lands on a shipped default. `wake_registry.rs:190` declares the `hey_fono.ort`
classifier — the default wake-word model — with `sha256: UNPINNED`, which `F-0008` already
records as accepting an unverified download. `F-0008` is about a hostile or wrong file
from the network; this is the same asset acquiring a corrupt file from an entirely
cooperative network, which needs no adversary at all — only an interrupted first download
and a CDN that does not do byte ranges.

The comment at `lib.rs:41-45` states the guarantee this breaks: the `.part` scheme exists
so *"`dest` never exists as a truncated/corrupt file"*. It prevents truncation. It does
not prevent duplication.

**Reproduction.** Start a model download, interrupt it partway so a `.part` remains, then
point the mirror at a server that answers `200` to a ranged request (any static file
server without range support). The resumed file is `existing + full_size` bytes. With a
pinned asset it is retried; with `hey_fono.ort` it is renamed into place.

**Suggested direction.** Treat the status as the authority on whether the resume was
honoured: if `existing > 0` and the response is `200`, the server sent the whole file, so
truncate and write from zero rather than appending. That is three lines and removes the
corruption for pinned and unpinned assets alike. Worth also verifying the `Content-Range`
start offset on a `206` before appending, since a server may honour the header and answer
from a different offset. `F-0008`'s suggested direction — pinning the default wake model
— closes the unpinned half independently, and the two fixes are complementary rather than
alternatives.

**Blast radius.** One branch in `try_download`.

## Unit O — S5 batch

- **`fono-download`'s HTTP client has no timeouts of any kind** (`lib.rs:102-104`) — no
  `connect_timeout`, no read deadline, no stall detection. A model fetch against an
  unresponsive host blocks until the OS TCP timeout, and a connection that stalls
  mid-transfer never returns at all. An overall timeout is genuinely wrong here (model
  files are hundreds of megabytes), but `fono-http`'s `read_body_with_watchdog` exists for
  exactly this shape and is already in the workspace, so an inter-chunk deadline is
  available for free. This is the same finding family as `F-L002`'s S5 batch.
- **Release verification is TLS-trust only, not a signature.** The `.sha256` sidecar is
  fetched from the same GitHub release as the asset (`lib.rs:593-602`), so anyone able to
  publish an asset can publish a matching digest. The code is honest about this — the
  no-sidecar path logs *"trusting Content-Length + TLS only"* (`lib.rs:617-621`) — and
  detached signing is a project-level decision rather than a code defect, so it is
  recorded here only so the trust boundary is written down somewhere.
- **`let _ = std::fs::remove_file(&backup);`** (`lib.rs:641`) discards the error from
  removing the previous `.bak`. On Unix the following rename overwrites it anyway, so the
  discard is harmless — but it throws away the earliest signal of exactly the
  directory-permission condition that makes `F-O001` fire.

## Unit O — lenses with no findings

- **The update path's ordering is otherwise correct throughout.** HTTPS is enforced on
  both the asset URL (`lib.rs:576-578`) and the sidecar (`lib.rs:686-688`); the announced
  `Content-Length` is checked against bytes received (`lib.rs:581-583`); a sidecar that
  exists but cannot be fetched is a hard failure rather than a downgrade to unverified
  (`lib.rs:596-601`), which is the correct fail-closed choice and the one most
  implementations get backwards; `0755` is set on the temp file **before** the swap
  (`lib.rs:626-631`) so the new binary is never briefly present and non-executable; and
  `--dry-run` returns before any rename (`lib.rs:633-635`).
- **Lens 2 (error paths), `fono-download` retry policy:** the three-attempt loop
  discriminates correctly between the two failure kinds — a transport error keeps the
  partial file so the next attempt resumes (`lib.rs:76-80`), a hash mismatch deletes it
  because resuming cannot repair a complete-but-wrong file (`lib.rs:66-67`). Both have the
  reasoning in a comment. Backoff is linear and bounded.
- **Lens 6 (resource lifetime), temp files:** every download in both crates streams to a
  sibling temp in the destination directory, so the final rename is same-filesystem and
  atomic, and an interrupted process leaves a `.part` or a dot-prefixed tempfile rather
  than a half-written target. `sha256_file` streams in 64 KiB chunks rather than reading
  the whole model into memory.

## Unit O — summary

| ID | Severity | One line |
|---|---|---|
| `F-O001` | S2 | A failed `persist` after the backup rename leaves no binary at the install path |
| `F-O002` | S2 | A server that ignores `Range` yields a duplicated-prefix file; unpinned assets keep it |

**Both findings are the same shape: a two-step operation with no handling for failure
between the steps.** `F-O001` moves the old binary away and then discovers it cannot put
the new one there; `F-O002` asks for a partial file and then writes whatever arrives as
though it were partial. In each case step one commits to an assumption that step two is
never asked to confirm.

**`F-O002` is the second finding to land on the unpinned default wake model**, after
`F-0008`. That asset is now implicated in two independent paths to a bad file on disk —
one adversarial, one purely accidental — which makes pinning it the single highest-value
change in this unit even though neither finding is *about* pinning.

---

# Stage 2 — Task 2.15: cross-unit synthesis

**Purpose.** A finding that appears in one unit is a bug. A finding that appears in eight
is a property of how the codebase is written, and fixing it eight times leaves the ninth
instance to be found by a user. This section names the patterns that recurred across three
or more units, and states the one structural change that would close each class rather
than its instances.

**Input.** 42 unit findings (`F-B`…`F-O`) plus 16 Stage-1 sweep findings (`F-0001`…
`F-0016`). Severity distribution across the whole ledger: **2 × S1, 16 × S2, 22 × S3,
1 × S4, 1 × S5** as headline severities, with further S5 items batched inside unit
sections.

## Pattern 1 — Silent substitution: an error becomes a value that means something else

**Units: B, C, A, F, M, N, O — seven of fourteen. The single most productive pattern in
the audit.**

Every instance has the same shape. A fallible operation fails, and instead of the failure
propagating, it is converted into a value that is indistinguishable from a legitimate
result — so the caller proceeds on a specific false claim about the world.

| Finding | Failure | Substituted value | The false claim |
|---|---|---|---|
| `F-M001` | `ydotool` exits non-zero | `Ok(())` | "the text was typed" |
| `F-B005` | `mcp_activity` poisoned | `0` | "no MCP interaction is active" |
| `F-A003` | capture buffer poisoned | `vec![]` | "the user recorded nothing" |
| `F-A003` | drain task panicked | `(vec![], ZERO)` | "the user recorded nothing" |
| `F-N001` | overlay thread dead | `()` | "the command was delivered" |
| `F-M002` | unknown backend name | `Self::None` | "the user chose clipboard" |
| `F-K001` | unknown config key | discarded | "the user did not set it" |
| `F-C001` | `Shutdown` never replies | `sent = false` | "the daemon did not receive it" |
| `F-F001` | resampler chunk errors | dropped | "there was no audio there" |

The mechanical measure: **471 `let _ = …` sites across 100 files**, concentrated in
`session.rs` (74), `daemon.rs` (27) and `fsm.rs` (26). The great majority are legitimate —
a channel send to a receiver that is deliberately allowed to be gone — but they are
spelled identically to the ones that are not, so no reviewer and no lint can tell them
apart.

**Why the class matters more than the instances.** Three of these are the audit's most
severe findings. `F-M001` destroys the user's dictation and reports success. `F-A003`
destroys it and blames the user, quoting a duration that contradicts its own message. In
both cases the *recovery mechanism already exists and is correct* — the clipboard
fallback, the failure notification — and is simply never reached, because the error that
would trigger it was converted into a success one layer down.

**Structural recommendation.** Not "fix nine sites". Adopt a convention that makes the two
kinds of discard visually distinct, and apply it as the sites are touched:

- `let _ = tx.send(…);` stays only where a dead receiver is the *expected, correct*
  outcome, and gains a trailing comment saying so.
- Everywhere else, the error is either propagated or logged once — `if let Err(e) = … {
  warn!(…) }`. For state that latches (a dead thread, a poisoned mutex), log on the
  *transition*, not per call, which is what makes `F-N001`'s fix one `AtomicBool`.
- For `std::sync::Mutex` specifically, adopt one poisoning policy project-wide (see
  Pattern 3).

A `clippy::let_underscore_must_use` deny is available but would fire on all 471; the
realistic path is the convention plus fixing the nine identified instances, which are
already located by ID.

## Pattern 2 — Sibling divergence: two implementations of one idea, one of them correct

**Units: B, C, A, A2, D, E, F, G, H, J, K, L, M — thirteen of fourteen. Present in every
unit but one.**

This codebase grew by extending working paths sideways, and the recurring defect is a
pattern copied without the part that made it work.

| Finding | The correct sibling | What the copy dropped |
|---|---|---|
| `F-B003` | `EnterAssistantLive` awaits inline, with a comment on the race | `Stop*` arms spawn |
| `F-C004` | speak loop breaks on EOF only, with a comment on stray bytes | hold loop breaks on any read |
| `F-A002` | focus probe uses `spawn_blocking` for 5 ms | device open blocks inline |
| `F-A006` | live, assistant and MCP all cancel | batch has no mechanism |
| `F-A007` | language guard is script-aware | boundary check is ASCII-only |
| `F-F002` | `SilenceWatch` has arm/resume hysteresis | live mode copied the thresholds only |
| `F-G003` | `frame.rs` bounds three lengths | `fono-ipc` bounds none |
| `F-H001` | `auth.rs` fails closed, 8 tests | Wyoming never calls it |
| `F-H002` | web settings uses `secrets.resolve` | Wyoming uses `env::var` |
| `F-J001` | polish sets `n_batch = context` | assistant caps at 2048, prefills 8192 |
| `F-K001` | 6 newest config structs deny unknown fields | 34 older ones do not |
| `F-L002` | four `warm_client`s | four different timeout policies |
| `F-M001` | enigo and XTEST propagate errors | three subprocess backends do not |
| `F-N001` | unknown `FONO_OVERLAY_BACKEND` warns | unknown `[inject].backend` is silent |

**The codebase already diagnosed this pattern once.** `crates/fono-core/src/llama_gen.rs`
exists because the polish backend fixed two generation bugs and *"the assistant kept its
own copy of the decode loop and shipped without either fix — observed as a refusal
sentence repeated to the 384-token cap (~13 s)."* The response was correct: extract one
definition both backends must use. `F-J001` is the same divergence in the batching, which
that extraction did not cover.

**Structural recommendation.** Where a second copy exists, delete it in favour of the
first — most of these findings *remove* code. Concretely, the highest-value extractions:
one `hold_until_eof` helper (`F-C004`), one HTTP client builder taking an explicit
workload descriptor (`F-L002`), `SilenceWatch` used by live mode (`F-F002`), and
`auth::decide` called by the Wyoming listener (`F-H001`). Each is net-negative on line
count and **net-zero on binary size** — every target crate is already a dependency.

## Pattern 3 — Unbounded waits and unbounded allocations: trusting the peer

**Units: C, G, H, L, O — five of fourteen, and they compound.**

The daemon models peers as either well-behaved or crashed. The failure it does not model
is the peer that stays alive and stops cooperating.

*Waits with no bound:* `F-C003` (speak lock held until EOF, no timeout — one hung agent
silences every other), `F-C005` (first IPC frame read has no timeout; `fono-ipc` contains
no `Duration` at all), plus the S5 items — `fono-download` has no timeouts of any kind,
`fono-tts/discovery.rs` defines a timeout constant and never applies it,
`mcp_client.rs` omits `connect_timeout` where every other client sets 5 s.

*Allocations sized by the peer's claim rather than its delivery:* `F-G001` (header limit
checked *after* `read_line` completes — a peer that never sends `\n` exhausts RAM),
`F-G002` (64 MiB reserved from a ~110-byte header), `F-G003` (IPC has no cap at all —
`FF FF FF FF` requests 4 GiB), plus `sway_focus` allocating a compositor-declared `u32`.

*And no cap on how many peers may do this at once:* a workspace-wide search for
`Semaphore` returns **nothing**. Neither accept loop bounds concurrent connections.

**The compounding is the finding.** `F-G001` is bounded per connection only by RAM;
without a connection cap it is bounded by nothing. `F-G003`'s 4 GiB is one allocation;
with `F-C005`'s missing timeout it is one allocation *per connection, held indefinitely*.
And the listener where all of this is reachable from the LAN is the one `F-H001` shows has
no authentication.

**Structural recommendation.** Three changes, each small, that collectively convert this
class from unbounded to bounded:

1. A `Semaphore` on both accept loops. This alone caps `F-G001`, `F-G002`, `F-G003` and
   `F-C005` simultaneously and is the highest-leverage single change in the network
   surface.
2. Bound every decode by what arrives, not by what is claimed — `take(limit)` for the
   header, `take(n).read_to_end()` for the payload. `frame.rs` already demonstrates the
   check; the fix is to move it before the allocation.
3. An inter-chunk deadline on every long-lived read. `fono-http`'s
   `read_body_with_watchdog` is exactly this and is already in the workspace, so
   `fono-download` and the speak lock can adopt it for free.

## Pattern 4 — Controls that exist but do not fire

**Units: Stage 1 sweep, D, E, H, plus the tests noted in B and A.**

A configured, documented or written control that never executes. This was the whole shape
of Stage 1 and it recurred throughout Stage 2.

| Finding | The control | Why it never fires |
|---|---|---|
| `F-H001` | Wyoming bearer token | resolved, plumbed, stored — no read site |
| `F-C001` | installer settle-wait, 3 platforms | `sent` is always `false` |
| `F-D002` | `notify_triggered` | hardcoded `return false` |
| `F-E001` | `State::McpDriven` + 3 transitions | its actions have no producer |
| `F-E002` | 2 more `HotkeyAction` variants | no producer |
| `F-B004` | "Pause hotkeys" menu item | handler is a `debug!` |
| `F-0001` | comment-hygiene gate | regex expects digits, slices use letters |
| `F-0002` | `cargo deny check bans` | configured, CI never runs it |
| `F-0010` | criterion bench | compiled every run, executed never |
| `F-0012` | six feature flags | no gate compiles them |
| `F-H003` | `mirror_to_stdout` config field | no read site |

And the test-level variant, where a test passes while the mechanism it names is disarmed:
`F-B004` (asserts the menu *label*, never actuates it), `F-A004` (asserts the Romanian
shell gate with a fixture containing no hyphen — the one character that defeats it),
`F-A006` (the FSM tests are thorough and in a well-covered file, and still miss the
absent `(Processing, CancelPressed)` arm because they assert transitions that exist).

**Structural recommendation.** These are individually cheap and collectively worth a
mechanical gate, because they are all the same question: *does this thing have a
consumer?* Two sweeps are already written and should become `tests/check.sh` assertions
alongside the existing SPDX and comment-hygiene checks:

- Every `Config`-tree field has a read site outside `config.rs` (`F-H003`'s method — it
  found the inert field in seconds, and would have found `F-H001` with the stronger
  "read through a value" variant).
- Every enum variant in a dispatch type has a producer outside its own module (`F-E001`
  and `F-E002`'s method).

Note the lesson from `F-A006`: **coverage does not measure this.** The missing FSM arm is
in one of the best-covered files in the workspace. Coverage measures what code runs, not
what code is absent.

## Pattern 5 — Shutdown is the least-exercised path

**Units: B, C, N, O — four of fourteen, and the S2 density is high.**

The daemon has exactly one correct teardown path and it is the one only developers use.

- `F-B001` — the only shutdown source is `ctrl_c()`, which is SIGINT. Nothing in the
  workspace mentions SIGTERM, and all three shipped systemd units stop via SIGTERM.
- `F-B002` — tray Quit calls `process::exit(0)`, skipping the destructors that two
  comments in the same file promise will run.
- `F-C001` — `Request::Shutdown` is a third `process::exit`, and the only one that skips
  even the socket unlink.
- `F-N001` — `OverlayHandle::shutdown()` is never called by the daemon at all.
- `F-O001` — the updater's rollback artefact is created and then stranded on the failure
  path.

**Structural recommendation.** One shutdown channel, fed by SIGTERM, SIGINT, tray Quit and
`Request::Shutdown`, with `run()` returning normally so the existing destructors execute.
This closes three S2 findings at once, makes two false comments true, and is *less* work
than fixing any one of them separately — `F-C001`'s installer logic becomes correct as
written with no change to the installers. `tokio::signal::unix` is already linked, so
**net-zero on binary size**.

## Pattern 6 — The `std`/`tokio` sync-primitive mix

**Predicted by the plan; measured, and it is smaller than expected.**

40 `std::sync::Mutex`/`RwLock` sites across 11 files, 18 `tokio::sync` sites across 6.
Only **three files use both**: `daemon.rs` (13 std / 12 tokio), `assistant.rs` (4 / 1) and
`groq_streaming.rs` (2 / 1). No instance of the classic defect — a `std` guard held across
an `await` — was found in the units audited.

What the mix *did* produce is the poisoning inconsistency: `F-B005`, `F-C002` and `F-A003`
are four sites on `std::sync::Mutex` with four different policies (`unwrap_or(0)`,
`expect` × 2, `unwrap_or_default`), none of which is right for data that is two `Copy`
scalars. The correct handling exists in the codebase — `brain_tap.rs:633` treats a poisoned
capture mutex as recoverable and continues, in an `extern "C"` callback where a panic
would abort the process.

**Structural recommendation.** Not a migration. One policy, stated once: for
`std::sync::Mutex` guarding plain data, use `unwrap_or_else(PoisonError::into_inner)` —
the data cannot be torn, so the last written value is strictly better information than a
default. Where the value is a counter, prefer an atomic, which has no poisoning semantics
at all. That resolves `F-B005`, `F-C002` and half of `F-A003` as one decision.

## What the audit did *not* find

Worth recording, because the absences are load-bearing for how much confidence to place in
the rest.

- **No memory-safety defect** across 140 `unsafe` sites (Unit I). Pointer discipline is
  consistent: null checks before use, handles closed on every path, out-parameters
  validated, and layout assumptions asserted rather than assumed.
- **No secret logged anywhere** — a sweep of every logging macro across all 272 source
  files containing `api_key`, `secret`, `password` or `bearer` returns only key *names*
  and file *paths*. The one place a secret is printed is the deliberate show-once path in
  `fono keys create`, which says so.
- **No on-disk permission defect.** All four SQLite stores chmod the database *and* its
  `-wal`/`-shm` sidecars to `0600` and each has a test that creates the file `0644` and
  asserts it comes back `0600`. `atomic_write` sets the mode on the temp file *before*
  `persist`, so the file is never briefly world-readable.
- **No inconsistent error mapping across the sixteen cloud providers** — the expected
  defect in that surface. Classification is centralised; the divergence is one layer
  below, in transport configuration (`F-L002`).
- **No renderer bounds defect** across 2,729 lines of software rasteriser with hundreds of
  computed pixel indices. Every framebuffer write goes through `get_mut`; negative
  coordinates are filtered before the cast that would wrap them.

## Ranked structural recommendations

Ordered by findings closed per unit of work, not by severity.

| # | Change | Closes | Size impact |
|---|---|---|---|
| 1 | One shutdown channel (SIGTERM + SIGINT + tray Quit + `Request::Shutdown`) | `F-B001`, `F-B002`, `F-C001`, and enables `F-N001`'s shutdown call | net-zero |
| 2 | `Semaphore` on both accept loops | caps `F-G001`, `F-G002`, `F-G003`, `F-C005` | net-zero |
| 3 | Return `Err` on non-zero exit in `inject_subprocess` | `F-M001` (S1) — three lines | net-zero |
| 4 | One poisoning policy for `std::sync::Mutex` | `F-B005`, `F-C002`, half of `F-A003` | net-zero |
| 5 | Call `auth::decide` from the Wyoming listener + use `secrets.resolve` | `F-H001` (S1), `F-H002` | net-zero |
| 6 | Two "does it have a consumer?" sweeps in `tests/check.sh` | prevents Pattern 4 recurring | net-zero |
| 7 | One HTTP client builder in `fono-http` with an explicit workload descriptor | `F-L001`, `F-L002` | net-zero |

**Every item on this list is net-zero on binary size.** No structural recommendation in
this audit requires a dependency new to `Cargo.lock`. The two places where a new
dependency was considered — `tokio-util::CancellationToken` for `F-D001` and `subtle` for
`F-H001`'s constant-time compare — both have adequate in-tree alternatives (`notify_one`
and a hand-rolled compare respectively), and are recorded in those findings as needing
sign-off if preferred.

---

# Stage 3 — Task 3.1: de-duplication and merge

**Result: no merges performed. Five clusters identified and deliberately kept separate.**

The temptation is to collapse findings that share a cause. That would be wrong here,
because Stage 3 approves and fixes by ID, and in every cluster below the members have
*different* locations, *different* blast radii, and in two cases different severities — so
merging would force an all-or-nothing approval on work the user may reasonably want to
split.

What is recorded instead is the cluster membership, so an approver can accept a whole
cluster in one decision if they choose.

| Cluster | Members | Why kept separate |
|---|---|---|
| **Shutdown** | `F-B001`, `F-B002`, `F-C001`, plus `F-N001`'s S5 note | Three different call sites in two files; `F-C001` additionally spans three installer files. One fix serves all three, but each is independently verifiable. |
| **Mutex poisoning** | `F-B005`, `F-C002`, `F-A003` | `F-A003` is S2 (destroys a dictation and misreports it); the other two are S3. Merging would raise or lower a severity artificially. |
| **Peer-claimed allocation** | `F-G001`, `F-G002`, `F-G003`, plus `F-M001`'s S5 `sway_focus` note | Two crates, different severities, and `F-G003` needs a constant that does not exist yet while `F-G001` needs a reordering. |
| **Capture-slot race** | `F-B003` (daemon layer), `F-A001` (orchestrator layer) | *One defect seen from two layers* — already stated in both entries. Either fix closes it, which is precisely why both must stay visible: merging would hide the choice of layer. |
| **Sentence terminators** | `F-A007` (S3, the gate) and Unit A's S5 note (the predicate) | The S5 note is already subsumed and is marked as upgraded by `F-A007`. No action. |

**One correction applied.** `F-G002`'s title says *"A 100-byte header"*; the body computes
~110 bytes. The body is right. Left as-is rather than edited, to avoid touching the ledger
after the anchor — noted here instead.

---

# Stage 3 — Task 3.2: verification of every S1 and S2 finding

**Eighteen findings are at S1 or S2** (2 × S1, 16 × S2). This is the gate the plan calls
*"the single most important quality control on the whole programme"*. Every one is
resolved below into exactly one of three states.

**Method and its limits.** Six findings were reproduced with executable tests written into
the working tree, run, and then **deleted** — the no-fix discipline forbids leaving
anything outside `docs/audit/`, and the Stage-3 rules require a regression test to land in
the same commit as its fix, so banking them now would pre-empt an approval that has not
happened. Each entry below records the harness precisely enough to recreate in minutes.
`git status --porcelain` is clean apart from this ledger.

## Reproduced by executing code — 6 findings

### `F-M001` (S1) — CONFIRMED

Harness: `crates/fono-inject/tests/audit_repro_f_m001.rs`, plus a PATH shim at
`/tmp/audit-shim/ydotool` that prints the exact stderr real `ydotool` emits when
`ydotoold` is not running (`failed to connect socket /tmp/.ydotool_socket`) and exits `1`.
A second shim makes `wl-copy` fail so the clipboard cannot mask the result.

```
PATH=/tmp/audit-shim:$PATH FONO_INJECT_BACKEND=ydotool \
  cargo test -p fono-inject --test audit_repro_f_m001
```

Observed: `type_text_with_outcome` returned **`Ok(Typed("ydotool"))`**. The clipboard was
never contacted. The tool exited non-zero, typed nothing, and the caller was told the text
was delivered. **Stands at S1.**

### `F-A006` (S2) — CONFIRMED

Harness: `crates/fono-hotkey/tests/audit_repro_f_a006.rs`, driving the real FSM to
`State::Processing` and dispatching `HotkeyAction::CancelPressed`.

Observed: state **unchanged**, and the event channel returned `Err(Empty)` — no
`HotkeyEvent` of any kind was emitted. The control case (`CancelPressed` in
`State::Recording`) emitted `Ok(Cancel)` on the same harness, proving the dispatch
mechanism works and only this transition is absent. **Stands at S2.**

### `F-F001` (S2) — CONFIRMED

Harness: `crates/fono-audio/tests/audit_repro_f_f001.rs`, a 48 kHz → 16 kHz `Resampler`
fed 2,048 samples, then 1,000 more.

Observed: **660 output samples after 2,048 input, and 660 after 3,048** — the additional
1,000 input samples produced *zero* additional output and are unreachable, there being no
`flush`. **Stands at S2.**

### `F-G001` (S2) — CONFIRMED

Harness: `crates/fono-net-codec/tests/audit_repro_f_g001.rs`, feeding
`Frame::read_async` a newline-free byte stream.

Observed: **4 MiB accumulated in the header buffer against a 1 MiB
`MAX_HEADER_LINE_BYTES`** — four times over the limit, and bounded only because the test
chose to stop feeding. `HeaderTooLong` never fired. **Stands at S2.**

### `F-A004` (S2) — CONFIRMED

Harness: a temporary `#[test]` inside `crates/fono/src/session.rs` (the functions are
private), reverted with `git checkout` immediately after the run.

Observed: `gated_builtin_suffix` returned `Some(suffix)` for all three probes, including
plain English `"well - actually"` and Romanian `"asta - da, mergem maine"` with
`language = Some("ro")`. One hyphen defeats the gate; the language argument is never
consulted because `has_shell_syntax` short-circuits first. **Stands at S2.**

### `F-G003` (S3, verified opportunistically) — CONFIRMED

Harness: `crates/fono-ipc/tests/audit_repro_f_g003.rs`.

Observed: `fono-ipc` declares no frame-size constant (`false`); a 256 MiB declaration with
**zero** payload bytes sent caused the full buffer to be zeroed before `read_exact`
returned `Err("early eof")` in 12 µs. A `u32::MAX` header requests 3 GiB. Verified even
though S3, because it was cheap and it is the compounding partner of `F-G001`.

## Proven statically — deterministic, no execution needed — 10 findings

These are not "read carefully and it looks wrong". In each case the defect is a property of
the code's structure that a single command establishes, and the outcome does not depend on
timing, environment or input.

| Finding | Sev | The command, and what it establishes |
|---|---|---|
| `F-H001` | S1 | `grep -n auth_token crates/fono-net/src/wyoming/server.rs` returns **exactly two lines** — the declaration at `:67` and `auth_token: None` at `:139`. No comparison exists. A documented authentication control has no read site. |
| `F-B001` | S2 | `SIGTERM\|SignalKind\|unix::signal` across all 272 source files returns 56 hits, **all of them** `fono-core/src/locale.rs`'s unrelated locale-`SignalKind` enum. Zero signal handling. The three shipped units stop via SIGTERM. |
| `F-C001` | S2 | `daemon.rs:2030-2032` is `Request::Shutdown => { std::process::exit(0); }`; the match closes at `:2033` and `write_frame(&mut stream, &resp)` is at `:2034`. The arm diverges, so the reply is unreachable, so `request_any`'s `read_frame` sees EOF, so `sent` is `false` on every platform. |
| `F-J001` | S2 | `DEFAULT_BATCH_SIZE = 2048` at `:135`; `prefill_batch_capacity = self.context_size` at `:1153`, `:1634`, `:1752` — **all three** prefill sites. Default context is 8192. The polish sibling uses `with_n_batch(self.context_size)` at `:250` and `LlamaBatch::new(self.context_size, 1)` at `:279`, agreeing by construction. |
| `F-O001` | S2 | `update/lib.rs:642` renames `target` → `backup`; `:654` is `tmp.persist(&target).map_err(…)?`. No `match`, no `Drop` guard, no reverse rename on the error path. |
| `F-O002` | S2 | `download/lib.rs:113` accepts `is_success()`, which includes **200**; `:131` opens with `.append(existing > 0)`. A server ignoring `Range` answers 200 with the whole body and it is appended after the partial. |
| `F-B002` | S2 | `daemon.rs:1184-1187` is `remove_file` then `process::exit(0)`, against comments at `:332-334` and `:346-348` promising drop-based mDNS goodbye. `process::exit` runs no destructors — a language guarantee. |
| `F-B004` | S2 | `menu.rs:138` pushes the item unconditionally; the handler at `daemon.rs:1191-1193` is a lone `debug!`. `TrayState::Paused` has **zero producers** workspace-wide. |
| `F-H002` | S2 | `daemon.rs:3640` and `:3819` are `std::env::var(&cfg.auth_token_ref)`; `:3997` is `secrets.resolve(&cfg.auth_token_ref)`. `Secrets::resolve` reads `secrets.toml` first, so the first two cannot see a token written by `fono keys add`. |
| `F-M002` | S3 | `inject.rs:53` is `_ => Self::None`, the same value `"clipboard"` produces. Verified opportunistically alongside `F-M001`. |

## Marked theory-only — 4 findings, all downgraded

Per the Task 3.2 rule, an S1/S2 that cannot be reproduced is downgraded rather than
carried. All four **retain high confidence in the mechanism** — each is a real gap in the
code — but none had its *user-visible consequence* demonstrated, and that is what the S2
band claims.

| Finding | Was | Now | What a reproduction needs |
|---|---|---|---|
| `F-A001` | S2 | **S3, theory-only** | A sub-100 ms hotkey double-tap landing inside the 54-line window between `session.rs:2758` and `:2812`. The window is proven by reading; that it is *hit* in practice is not. The auto-mute inversion is the cheapest observable and needs a scripted press pair. |
| `F-A003` | S2 | **S3, theory-only** | A panic in the capture thread while it holds the buffer lock. Whether any holder can panic was never traced. The misreporting is certain *given* a poisoned lock; reachability is not established. |
| `F-D001` | S2 | **S3, theory-only** | A cancel arriving during the synchronous stretch between the pump's `notified()` arms. `notify_waiters`' lossiness is a documented tokio property; that the window is wide enough to hit in real use is unmeasured. |
| `F-L001` | S2 | **S3, theory-only** | A two-to-three-minute dictation against a live cloud STT provider — needs a paid key and a real uplink. The 45 s constant at `groq.rs:164` and the absence of any recording-length cap are both certain; the crossover point is not measured. |

**Two of these deserve a note.** `F-A001` was the finding that *resolved* `F-B003` — it
established that the orchestrator does not serialise start against stop. That structural
conclusion is unaffected by the downgrade; what is downgraded is the claim about
frequency. And `F-L001`'s downgrade is the weakest of the four: raising the constant to
match the project's own `wyoming.rs:42` value of 300 s is a one-line change with no
downside, so it may be worth approving on the static evidence alone.

## Verification summary

| State | Count | Findings |
|---|---|---|
| Reproduced by execution | 5 of the 18 | `F-M001`, `F-A006`, `F-F001`, `F-G001`, `F-A004` |
| Proven statically | 9 of the 18 | `F-H001`, `F-B001`, `F-C001`, `F-J001`, `F-O001`, `F-O002`, `F-B002`, `F-B004`, `F-H002` |
| Downgraded to theory-only | 4 of the 18 | `F-A001`, `F-A003`, `F-D001`, `F-L001` |
| **Unverified S1 remaining** | **0** | — |

Plus two S3 findings verified opportunistically (`F-G003`, `F-M002`).

**Revised severity distribution:** **2 × S1, 12 × S2, 26 × S3, 1 × S4, 1 × S5** headline,
with further S5 items batched in unit sections.

**Both S1s survived verification**, one by execution and one by a two-line grep, and
neither weakened. That is the outcome that matters most: the two findings the audit rates
highest are the two it can prove.

---

# Stage 3 — Task 3.3: ranked for approval

**Sort order:** severity, then fix cost, then blast radius — so within each severity band
the cheapest and most contained work comes first.

**Cost scale.** `XS` = under 5 lines. `S` = one function. `M` = one module or a handful of
call sites. `L` = a design decision or a cross-crate change.

**Dependency flag.** Every fix below is **net-zero on binary size** — no recommendation in
this audit requires a crate new to `Cargo.lock`. Two findings have an *optional* variant
that would (`F-D001` via `tokio-util`, `F-H001` via `subtle`); both are marked `[DEP]` and
both have an adequate in-tree alternative that is the recommended path. Nothing needs
size sign-off unless the optional variant is preferred.

## Band 1 — S1 (2 findings)

| # | ID | Cost | Blast radius | Fix | Size |
|--:|---|---|---|---|---|
| 1 | `F-M001` | **XS** | one helper, 3 backends | Return `Err` on non-zero exit in `inject_subprocess`; the existing clipboard fallback then fires | net-zero |
| 2 | `F-H001` | **M** | one handler + docs | Call `auth::decide` from the Wyoming listener; fix `F-H002` in the same change | net-zero `[DEP]` optional |

**`F-M001` is the whole audit's best ratio** — three lines, S1, reproduced by execution,
and it stops the product destroying dictations while reporting success. If exactly one
finding is approved, this is it.

**`F-H001` must be approved together with `F-H002`** or the result is worse than today: a
listener that rejects users who configured the token the documented way. Recommended
constant-time compare is hand-rolled (a few lines); `subtle` is the `[DEP]` variant and is
not needed.

## Band 2 — S2 (12 findings)

| # | ID | Cost | Blast radius | Fix | Size |
|--:|---|---|---|---|---|
| 3 | `F-B004` | **XS** | 2 lines + 1 test line | Stop rendering "Pause hotkeys" until it works — or implement it, the state and icon already exist | net-zero |
| 4 | `F-O002` | **XS** | one branch | If `existing > 0` and the status is 200, truncate instead of appending | net-zero |
| 5 | `F-H002` | **XS** | 2 lines | `secrets.resolve` at `daemon.rs:3640` and `:3819` | net-zero |
| 6 | `F-A004` | **S** | 2 const entries + 1 predicate | Anchor `"--"` / `" -"` as flag tokens; add a hyphen-bearing Romanian fixture | net-zero |
| 7 | `F-O001` | **S** | one `match` or guard struct | Rename `backup` back to `target` when `persist` fails | net-zero |
| 8 | `F-G001` | **S** | one function | `take(MAX + 1)` on the header read so the bound precedes the allocation | net-zero |
| 9 | `F-A006` | **S** | 1 FSM arm + 1 stored handle | Add `(Processing, CancelPressed)`; retain the pipeline's `AbortHandle` | net-zero |
| 10 | `F-B001` | **S** | 1 `select!` arm | SIGTERM arm breaking to the existing clean path | net-zero |
| 11 | `F-B002` | **S** | 1 channel + 2 arms | Tray Quit signals shutdown instead of `process::exit` | net-zero |
| 12 | `F-C001` | **S** | 1 arm | Reply `Ok` before exiting; the three installers become correct unchanged | net-zero |
| 13 | `F-F001` | **M** | 1 method + 4 call sites | Add `flush()`; give the capture-side resampler an owner that outlives the stream | net-zero |
| 14 | `F-J001` | **M** | 1 helper, 3 call sites | Chunk the prefill into `n_batch` decodes | net-zero |

**Items 10, 11 and 12 are one change.** One shutdown channel fed by SIGTERM, SIGINT, tray
Quit and `Request::Shutdown`. Approving them separately triples the work; approving them
together is `S`, not `3 × S`, and it makes two currently-false comments true.

**Item 13's cost is in the lifetime, not the flush.** The capture resampler currently
lives inside the cpal callback closure and is dropped with the stream, so it has no owner
to flush at stop time. That is the larger half.

## Band 3 — S3 (26 findings)

Not tabulated individually; grouped by the structural change that closes each set, since
approving a group is cheaper than approving its members.

| Group | Members | Cost | Fix |
|---|---|---|---|
| Peer-claimed allocation | `F-G002`, `F-G003` | S | Bound by bytes delivered, not bytes claimed; add `MAX_FRAME_BYTES` to `fono-ipc` |
| Connection caps | `F-C003`, `F-C005` | S | One `Semaphore` per accept loop + a header-read timeout |
| Mutex poisoning | `F-B005`, `F-C002`, `F-A003` | S | One policy: `PoisonError::into_inner`, or an atomic for counters |
| Dead FSM surface | `F-E001`, `F-E002` | M | Pick one MCP-cancellation design and delete the other; ~55 lines net removed |
| Sibling consolidation | `F-C004`, `F-F002`, `F-L002` | M | One `hold_until_eof`, `SilenceWatch` in live mode, one HTTP client builder |
| Silent value substitution | `F-M002`, `F-N001`, `F-K001` | S–M | Warn on unrecognised input; latch and log a dead overlay thread once |
| Text-path correctness | `F-A007`, `F-A005` | S | Unicode sentence terminators; make `window_title_regex` a regex or rename it |
| Config integrity | `F-C006` | M–L | Advisory file lock across load→mutate→save (cross-process; an in-process mutex is insufficient) |
| Async hygiene | `F-A002`, `F-J002` | S | `oneshot` for the device-open handshake; correct the cache-key version label |
| Theory-only (downgraded) | `F-A001`, `F-A003`, `F-D001`, `F-L001` | XS–M | See note below |
| Remaining | `F-B003`, `F-B006`, `F-I001` | XS–M | `F-B003` closes with `F-A001`; `F-B006` move the guard next to the bind; `F-I001` run one `nm -D` and record the answer |

**On the four theory-only findings.** Three should wait for a reproduction. `F-L001` is the
exception worth approving on static evidence: raising the 45 s constant to the project's
own 300 s (`wyoming.rs:42`) is one line with no downside and converts a lost dictation into
a slow one.

**`F-I001` is the cheapest item in the audit** — run `nm -D` on the release binary. If the
`vk*` symbols are not dynamically exported, the risk is nil and the fix is a comment.

## Band 4 — S4/S5 and the Stage-1 sweep findings

| ID | Cost | Note |
|---|---|---|
| `F-D002` | XS | Delete `notify_triggered` and both call sites — it is hardcoded `false` and disguises the gap `F-D001` exploits |
| `F-0001` | XS | Widen the comment-hygiene regex to letters; two legitimate collisions need care |
| `F-0002` | XS | `allow-wildcard-paths = true`, then add `bans` to the CI `cargo deny` line |
| `F-0008` | XS | Pin the default wake model's SHA — implicated in two independent paths to a bad file (`F-0008`, `F-O002`) |
| `F-H003` | XS | Delete `mirror_to_stdout`, or implement against stderr — never stdout, which is the JSON-RPC transport |
| `F-0009` | XS | Remove `docs/**` from `paths-ignore`, or move the bench baselines |
| `F-0004` | S | `#![forbid(unsafe_code)]` on the 12 crates with none — shrinks the reviewable surface from 19 files to 7 |
| Two new gates | M | "Every config field has a read site" and "every dispatch variant has a producer" in `tests/check.sh` — both sweeps are already written and each found a real defect |

## Recommended first tranche

If a subset is wanted rather than the whole ledger, this is the highest value for the
least risk — **eight IDs, all XS or S, all net-zero on size**:

1. `F-M001` — S1, three lines, reproduced.
2. `F-H001` + `F-H002` — S1, must go together.
3. `F-B001` + `F-B002` + `F-C001` — three S2s as one shutdown channel.
4. `F-O002` — S2, one branch.
5. `F-B004` — S2, the only finding a user can see today.

That is five S2s and both S1s, and no item in it touches a design decision.

---

# Stage 3 — Task 3.5: execution rules (recorded, not yet executed)

Binding on whoever implements an approved finding.

1. **One finding, one commit.** The commit body names the ID. No commit fixes two IDs
   unless they were approved as a group (items 10–12, and `F-H001`+`F-H002`).
2. **A regression test lands in the same commit as its fix.** For the five findings
   reproduced in Task 3.2, the harness is described precisely enough to recreate; it must
   be recreated and *kept* this time.
3. **The full pre-commit gate runs before every commit** — `cargo fmt --all -- --check`,
   `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace
   --tests --lib`, all under `nice -n 10`.
4. **`./tests/check.sh --size-budget` runs before any push**, per the project rules.
5. **No unapproved fix, however small.** Spotting a one-line bug adjacent to an approved
   fix means recording a new finding ID, not fixing it.
6. **Commit messages follow the project's user-facing rule** — describe the behaviour that
   changed for the user, not the mechanism, and never cite a plan file or a finding ID in
   the subject line.
