# Wave 2 — Close out Half-Shipped Plans + Tighten CI Bench Gate

## Status: Landed 2026-04-28

All three threads delivered:

- **Thread A** — `76b9b08` `feat(fono-bench): typed ModelCapabilities + split equivalence/accuracy thresholds`
- **Thread B** — `87221a2` `feat(fono-update): per-asset sha256 sidecar verification + --bin-dir`
- **Thread C** — same-session commit `ci(fono-bench): real-fixture equivalence gate with tiny.en + baseline JSON anchor`

`docs/status.md` carries the full landing report. `docs/plans/2026-04-25-fono-roadmap-v2.md` R5.1 + R5.2 ticked. Recommended-next-session block points at Wave 3 (Slice B1).

## Objective

Finish three loosely-coupled threads that the doc-reconciliation pass
(`plans/2026-04-28-doc-reconciliation-v1.md`) carried forward:

1. **Equivalence harness typed-capability surface** — replace the
   inline `english_only` boolean at
   `crates/fono-bench/src/bin/fono-bench.rs:339` with a typed
   `ModelCapabilities` value, split per-fixture
   `levenshtein_threshold` into separate `equivalence_threshold` /
   `accuracy_threshold` so the two gates can be tightened
   independently, persist `model_capabilities` into
   `EquivalenceReport`, and add the mock-STT capability-skip test.
2. **Self-update finishing pass** — per-asset `.sha256` sidecar
   verification in `apply_update`, `--bin-dir` CLI flag, the
   release workflow emits a `.sha256` per asset, and a manual QA
   checklist at `docs/dev/update-qa.md`.
3. **Real-fixture CI bench gate** — replace the `cargo bench
   --no-run` compile-sanity step at
   `.github/workflows/ci.yml:64-68` with a fixture-driven
   `fono-bench equivalence` run against the committed manifest, and
   commit `docs/bench/baseline-comfortable-tiny-en.json` as the
   per-PR comparison anchor (R5.2 from `roadmap-v2`).

The three threads are deliberately scoped to be independent: any one
can land in isolation without blocking the others. Final session
delivers three DCO-signed commits.

This plan **modifies Rust source code, manifest files, CI YAML, and
release-workflow YAML.** The doc-only constraint of the previous
wave is lifted.

## Background — what is already in place vs what is missing

### Equivalence harness (Thread A)

Already shipped (verified at HEAD `4517133` post-reconciliation):

- `Metrics.stt_accuracy_levenshtein: Option<f32>` field
  (`crates/fono-bench/src/equivalence.rs:113-114`).
- Two-gate `decide_verdict` function and unit-test
  `decide_verdict_two_gates` (`equivalence.rs:556-567` and
  `:902-922`).
- `acc` column in `print_table`
  (`crates/fono-bench/src/bin/fono-bench.rs:527`).
- Capability skip pre-inference for English-only models on
  non-English fixtures (`bin/fono-bench.rs:339, 357-385`).
- Per-fixture `levenshtein_threshold` populated for every multilingual
  fixture in `tests/fixtures/equivalence/manifest.toml`.
- Back-compat test
  `metrics_back_compat_deserializes_without_accuracy_field`
  (`equivalence.rs:927`).
- `tests/bench.sh` runner with multilingual fixtures (commit
  `b6596c0`).

Still missing relative to the original plan:

- Typed `ModelCapabilities` value in
  `crates/fono-bench/src/capabilities.rs`. Today the capability
  decision is an inline boolean
  (`args.stt == "local" && args.model.ends_with(".en")` at
  `bin/fono-bench.rs:339`).
- Separate `accuracy_threshold` field on `ManifestFixture` —
  currently the same `levenshtein_threshold` is reused for both
  the equivalence and accuracy gates (`equivalence.rs:488-490`,
  `:514`). This conflates two independent measurements; tightening
  one without the other is impossible.
- `requires_multilingual: Option<bool>` field on `ManifestFixture`
  (defaults to `language != "en"`). Today the requirement is
  derived inline at `bin/fono-bench.rs:357`.
- `EquivalenceReport.model_capabilities: Option<ModelCapabilities>`
  block — today the report has no record of which model produced it
  beyond `stt_backend: String` and `tier: String`.
- A regression test that builds an STT mock that **panics** on
  `transcribe(...)` and asserts the capability skip path returns
  `Verdict::Skipped` without invoking it. Today's tests cover the
  decision function but not the orchestration around it.
- `EquivalenceReport::overall_verdict()` ignoring capability-induced
  Skipped rows. Currently the function (`equivalence.rs:163-180`)
  treats *every* Skipped row identically: `Pass` when at least one
  row passed, `Skipped` when every row skipped. The plan's intent —
  capability skips should never produce a `Skipped` overall when
  some rows ran successfully — happens to be satisfied today as a
  side effect; we want that to be a typed contract rather than an
  accident.

### Self-update (Thread B)

Already shipped (commit `3e2c742`):

- `crates/fono-update/src/lib.rs` foundations, GitHub Releases poll,
  `UpdateInfo` / `UpdateStatus` (`:31-107`).
- Background checker thread, on-disk cache, `[update]` config knobs,
  `FONO_NO_UPDATE_CHECK` env var (`crates/fono/src/daemon.rs:145-185`,
  `crates/fono-core/src/config.rs:47, 70`,
  `crates/fono-update/src/lib.rs:267`).
- Tray "Update to <tag>" entry (`crates/fono-tray/src/lib.rs:78,
  487-494`; daemon hook at `crates/fono/src/daemon.rs:476, 514,
  1195-1213`).
- `apply_update` with HTTPS enforcement, content-length check,
  SHA-256 of stream, atomic same-dir rename, `.bak` rollback
  sidecar (`crates/fono-update/src/lib.rs:381-477`).
- `restart_in_place` via `execv` on Unix (`:507-529`).
- `is_package_managed` heuristic for `/usr/bin`, `/bin`,
  `/usr/sbin` (`:346-352`).
- `fono update [--check] [-y] [--dry-run] [--channel] [--no-restart]`
  CLI surface (`crates/fono/src/cli.rs:235-260, 1248-...`).

Still missing relative to the original plan:

- **Per-asset `.sha256` sidecar verification.** Today `apply_update`
  computes the SHA-256 of the streamed bytes
  (`crates/fono-update/src/lib.rs:425, 479-501`) but never compares
  against an authoritative published digest. A tampered mirror could
  serve a different binary that also matches the GitHub-announced
  `Content-Length`; the user is currently trusting TLS-to-GitHub
  alone.
- **`--bin-dir <path>` CLI flag** for forcing the install directory
  (matches `BIN_DIR` semantics from the install script). Today the
  CLI exposes neither `--bin-dir` nor `--target-override`; only the
  internal `ApplyOpts.target_override` exists
  (`crates/fono-update/src/lib.rs:362-363`).
- **Release workflow emits a `.sha256` per asset** alongside the
  aggregate `SHA256SUMS` file (Task 20 of the original self-update
  plan). Today `release.yml:327-335` writes `SHA256SUMS` only.
  Optionally publish each `<asset>.sha256` so older clients can
  fetch a single file without parsing the aggregate.
- **`docs/dev/update-qa.md` checklist** (Task 22 of the original
  plan). No automation; the checklist guards the maintainer's
  manual verification when self-update changes ship.

### CI bench gate (Thread C)

Already shipped:

- `.github/workflows/ci.yml:64-65` runs `fono-bench` ignored latency
  smoke tests on every PR.
- `:67-68` runs `cargo bench -p fono-bench --no-run` (compile-only
  sanity).

Still missing:

- **Real-fixture equivalence run on every PR**, with a typed-verdict
  comparison that fails the build on regression. The plan's intent
  is for the harness to be a **gate**, not a smoke check; today it
  only runs locally via `tests/bench.sh`.
- **`docs/bench/baseline-comfortable-tiny-en.json`** committed as
  the comparison anchor (R5.2 of `docs/plans/2026-04-25-fono-roadmap-v2.md`).
- Decision: do we run the gate against `tiny.en` (small, fast,
  deterministic, English-only fixtures pass / non-English skip) or
  against `small` multilingual (covers the full manifest but is 4x
  slower and may be flaky on shared CI runners)? Recommendation:
  **`tiny.en` for the per-PR gate**, reserve `small` for a
  scheduled nightly job. Justification: PR latency budget. The
  English-only-skip behaviour is exactly what we want a per-PR gate
  to enforce.

## Implementation Plan

### Phase 0 — Verification baseline

- [ ] Task 0. Confirm `cargo build --workspace`, `cargo test
  --workspace --all-targets`, and `cargo clippy --workspace
  --all-targets -- -D warnings` are green at HEAD before any edit.
  Capture output. If any of the three fails on `main` for a reason
  unrelated to this plan, **stop and report**: this plan assumes a
  green baseline.

### Thread A — Typed `ModelCapabilities` + split thresholds

#### Phase A1 — `ModelCapabilities` value type

- [ ] Task A1. Create `crates/fono-bench/src/capabilities.rs`
  exposing:
  - `pub struct ModelCapabilities { pub english_only: bool, pub
    model_label: String }` with `#[derive(Debug, Clone, Serialize,
    Deserialize, PartialEq, Eq)]`.
  - `impl ModelCapabilities`:
    - `pub fn for_local_whisper(model_stem: &str) -> Self` —
      normalise the stem by stripping a trailing
      `-q\d+(_\d+)?` quantization fragment via a small regex
      (use `regex::Regex` already in the workspace at
      `crates/fono-stt/Cargo.toml`; add to `fono-bench` if not
      present; or implement the strip with manual char scanning
      to avoid the dep), then `english_only =
      normalised.ends_with(".en")`, `model_label =
      format!("local:{model_stem}")`.
    - `pub fn for_cloud(provider: &str, model: &str) -> Self` —
      explicit per-provider arms (`groq`, `openai`,
      `assemblyai`, `deepgram`, `azure`, `google`,
      `speechmatics`, `cartesia`, `nemotron`); all return
      `english_only = false` today. Unknown providers warn and
      default `english_only = false`.
    - `pub fn fixture_requires_multilingual(fx_lang: &str,
      fixture_override: Option<bool>) -> bool` — the derived
      default `fx_lang != "en"`, with explicit override taking
      precedence.
- [ ] Task A2. `pub mod capabilities;` re-export from
  `crates/fono-bench/src/lib.rs` so the bin and integration tests
  share the type.
- [ ] Task A3. Unit tests in `capabilities.rs`:
  - `tiny.en`, `small.en`, `medium.en` → `english_only = true`.
  - `tiny`, `base`, `small`, `medium`, `large-v3`,
    `large-v3-turbo` → `english_only = false`.
  - Quantization-suffixed stems: `tiny.en-q5_1`, `tiny-q4_0` →
    correct decision after normalisation.
  - `for_cloud("groq", "whisper-large-v3")`,
    `for_cloud("openai", "whisper-1")` → multilingual.
  - `for_cloud("future-en-only", "x")` → multilingual + warn (use
    `tracing::warn!` and a test-side capture if convenient; if
    capturing logs is fiddly, drop the warn assertion and just
    check the boolean).
  - `fixture_requires_multilingual("en", None)` → `false`;
    `("ro", None)` → `true`; `("en", Some(true))` → `true`;
    `("ro", Some(false))` → `false`.

#### Phase A2 — Manifest schema split

- [ ] Task A4. Extend `ManifestFixture` (`crates/fono-bench/src/equivalence.rs:40-66`):
  - Add `pub equivalence_threshold: Option<f32>` with
    `#[serde(alias = "levenshtein_threshold", default)]` so existing
    fixtures keep parsing.
  - Add `pub accuracy_threshold: Option<f32>` with `#[serde(default)]`.
  - Add `pub requires_multilingual: Option<bool>` with
    `#[serde(default)]`.
  - **Keep `pub levenshtein_threshold: Option<f32>` for one cycle**
    as a deprecated alias source; update `Cargo.toml` /
    in-file comment to flag it as such. Implementation: drop the
    `levenshtein_threshold` field in favour of the alias path
    above, since `serde(alias)` reads either name into
    `equivalence_threshold` automatically. Confirm during
    implementation that `serde(alias = "levenshtein_threshold")`
    behaves correctly when both names are present (it errors —
    that's fine; the manifest never carries both).
- [ ] Task A5. Update `tests/fixtures/equivalence/manifest.toml`:
  - Rename every `levenshtein_threshold = X` to
    `equivalence_threshold = X` (the alias accepts either, but the
    canonical name is now `equivalence_threshold`).
  - For each fixture, populate `accuracy_threshold` independently:
    - English fixtures (`en-single-sentence`, `en-multi-sentence`,
      `en-narrative-pause`, `en-conversational`):
      `accuracy_threshold = 0.20` for the non-`en-single-sentence`
      ones; `en-single-sentence` keeps its 1.0 informational
      threshold (whisper-small truncation). Equivalence threshold
      stays as currently set.
    - Spanish (`es-lorca-reyerta`): `accuracy_threshold = 0.30`.
    - French (`fr-gide-symphonie`): `accuracy_threshold = 0.30`.
    - Chinese (`zh-luxun-kuangren`): `accuracy_threshold = 0.50`
      (informational only; CJK + streaming-mojibake known issues).
    - Romanian (`ro-talcuirea-matei`, `ro-man`, `ro-woman`):
      `accuracy_threshold = 0.30`.
  - **Keep the existing `equivalence_threshold` values for
    backwards-compat**: tightening either side is a follow-up
    (Wave 5 Task 18 in the strategic plan), not this wave. The goal
    of A4/A5 is *separability*, not new tightness.
  - Add `requires_multilingual = true` explicitly **only** when
    the derived default would be wrong; in the current manifest no
    overrides are needed (every non-`en` fixture is multilingual,
    every `en` fixture is not).

#### Phase A3 — Plumb the typed surface through `run_fixture`

- [ ] Task A6. Change `run_fixture`
  (`crates/fono-bench/src/equivalence.rs:416-422`) signature to
  accept a `&ModelCapabilities`. The capability decision moves
  from inline boolean (`bin/fono-bench.rs:357`) into the function
  itself, so the harness can be tested in isolation:
  ```rust
  pub async fn run_fixture(
      fixture: &ManifestFixture,
      fixture_root: &Path,
      stt: Arc<dyn SpeechToText>,
      streaming_stt: Option<Arc<dyn StreamingSttHandle>>,
      caps: &ModelCapabilities,
      quick_max_seconds: Option<f32>,
  ) -> Result<EquivalenceResult>
  ```
- [ ] Task A7. Inside `run_fixture`, before any `stt.transcribe`
  call, evaluate
  `caps.english_only &&
   ModelCapabilities::fixture_requires_multilingual(&fixture.language,
   fixture.requires_multilingual)`. When true, return a `Skipped`
  result with note `"model {label} is English-only; fixture
  language is {lang}"`. The wav read can be skipped entirely (cheap
  but not free).
- [ ] Task A8. After the existing batch + streaming + accuracy
  computation, evaluate `decide_verdict` against the **two distinct
  thresholds**:
  - `equiv_threshold = fixture.equivalence_threshold.unwrap_or(TIER1_LEVENSHTEIN_THRESHOLD)`.
  - `acc_threshold = fixture.accuracy_threshold.unwrap_or(equiv_threshold)`
    — when no separate accuracy threshold is set, fall back to the
    equivalence threshold so existing behaviour is preserved.
  - Update `decide_verdict` signature to `(equiv: Option<f32>,
    accuracy: Option<f32>, equiv_threshold: f32, acc_threshold: f32)
    -> Verdict`.
  - Update note construction at `equivalence.rs:503-517` to print
    the gate-specific threshold each one tripped on.
- [ ] Task A9. Remove the inline capability check at
  `bin/fono-bench.rs:339` and the duplicated Skipped builder at
  `:357-385`. The bin-side resolves capabilities once via
  `ModelCapabilities::for_local_whisper(&args.model)` (or
  `for_cloud` once cloud STT is wired up) and threads the value
  into each `run_fixture` call. The bin-side error builder at
  `:413-435` (cooked fixture failure) is unchanged.

#### Phase A4 — Report-level capability persistence

- [ ] Task A10. Add `pub model_capabilities: Option<ModelCapabilities>`
  to `EquivalenceReport` (`equivalence.rs:147-159`) with
  `#[serde(default)]` for back-compat. Populate from the resolved
  `ModelCapabilities` once per run.
- [ ] Task A11. Update
  `EquivalenceReport::overall_verdict` (`equivalence.rs:163-180`) to
  classify Skipped rows as **capability-induced** (`note.contains(
  "is English-only")` is the simple fingerprint, or — better — add a
  typed `skip_reason: Option<SkipReason>` to `EquivalenceResult`
  with variants `Capability`, `Quick`, `NoStreaming`,
  `RuntimeError`). Implementation note: introducing
  `SkipReason` is the cleaner path; do that. Update the Skipped
  builders in `bin/fono-bench.rs` and `equivalence.rs::skipped` to
  set the correct variant.
- [ ] Task A12. With `SkipReason` in place, `overall_verdict`
  becomes: a run is `Pass` when every non-skipped row passes; a
  run is `Skipped` only when **every** row is skipped *and* none of
  them are `SkipReason::Capability` — i.e. the user ran the harness
  on hardware/config that simply has no executable rows. Capability
  skips alone do not make a run `Skipped`. A pure capability-skip
  run with zero non-skipped rows reports `Pass` with a note.

#### Phase A5 — Mock-STT capability-skip integration test

- [ ] Task A13. Add
  `crates/fono-bench/tests/capability_skip.rs` (integration test —
  needs `fono-bench` + `fono-stt` as dev deps; both already in
  workspace). Test:
  - Build a `PanicStt` that implements `SpeechToText` but
    `panic!("must not be invoked")` from `transcribe`.
  - Construct a single `ManifestFixture { language: "ro", … }`.
  - Resolve `ModelCapabilities::for_local_whisper("tiny.en")` →
    `english_only = true`.
  - Call `run_fixture(&fixture, &fixture_root, panic_stt, None,
    &caps, None)`.
  - Assert: result is `Verdict::Skipped`, `skip_reason` is
    `Some(SkipReason::Capability)`, the note contains `"English-only"`,
    and `metrics.stt_accuracy_levenshtein.is_none()` (no accuracy
    measurement either, since we never ran inference).
  - The fixture file doesn't need to exist on disk — the function
    short-circuits before opening the WAV. If the wav-read happens
    *before* the capability check today, move it to *after* (Task
    A7 already implies this).
- [ ] Task A14. Add a sibling `tests/two_gate_verdict.rs` integration
  test that drives `run_fixture` end-to-end with a synthetic STT
  that returns a fixed transcript and a fixture whose `reference`
  text intentionally diverges by ~20% so the accuracy gate fails
  while the (absent) equivalence gate doesn't fire. Asserts
  `Verdict::Fail` with note `"acc"` substring. Use one of the
  existing 5-second WAV fixtures (`en-single-sentence`) so no new
  audio is committed.

#### Phase A6 — Plan checkbox close-out

- [ ] Task A15. Tick the remaining open boxes in
  `plans/2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md`:
  Tasks 1, 2, 3 (capabilities resolver), 4, 5, 6 (manifest
  fields), 9, 10 (two-gate verdict + sub-verdict notes), 11
  (resolved-once threading), 13 (`model_capabilities` block), 14
  (overall verdict), 15 (mock-STT + two-gate tests). Update its
  `## Status` header to `Status: Landed in <commit-A>; full plan
  delivered`.

### Thread B — Self-update finishing pass

#### Phase B1 — `.sha256` sidecar verification in `apply_update`

- [ ] Task B1. Extend `UpdateInfo` (`crates/fono-update/src/lib.rs:31-107`)
  with `pub sha256_url: Option<String>` and
  `pub expected_sha256: Option<String>`. Populate these in
  `fetch_latest` (`:240-262`): for each release, look for a
  sibling asset named exactly `<asset_name>.sha256`. When present,
  set `sha256_url`; when the corresponding response body is small
  enough (< 1 KB), pre-fetch it during `fetch_latest` and store
  the parsed hex digest in `expected_sha256`. Otherwise leave
  `expected_sha256 = None` and resolve at apply time.
- [ ] Task B2. In `apply_update` (`:381-477`), after `stream_download`
  returns the computed SHA-256:
  - If `info.expected_sha256.is_some()`, compare; on mismatch,
    `anyhow::bail!` and **leave the original binary untouched**
    (we never renamed it yet — failing here is safe).
  - If `info.expected_sha256.is_none()` but `info.sha256_url.is_some()`,
    fetch the sidecar inline (same `download_client` reqwest), parse
    `<hex> <filename>` or bare `<hex>`, compare. Same failure
    semantics.
  - If neither field is set, log a `warn!` (`"no .sha256 sidecar
    published for {tag}; trusting Content-Length + TLS"`) and
    proceed. Do **not** fail closed — the v0.1.0 / v0.1.1 / v0.2.0
    / v0.2.1 releases predate the sidecar publication and existing
    users must still be able to update from them.
- [ ] Task B3. Unit-test the sidecar parser (a small free function
  `parse_sha256_sidecar(body: &str, expected_filename: &str) ->
  Result<String>`): tolerate `<hex>\n`, `<hex>  <name>\n`,
  `<hex> *<name>\n`, multi-entry sidecars (pick the one whose
  filename matches), trailing whitespace. Reject too-short / non-hex
  digests.

#### Phase B2 — `--bin-dir` CLI flag

- [ ] Task B4. Add `bin_dir: Option<PathBuf>` to the `Update` clap
  variant in `crates/fono/src/cli.rs:235-260`:
  ```
  /// Override the install directory. Useful when running with
  /// elevated privileges and the autodetected `current_exe()` is
  /// in `/usr/local/bin/fono`. Equivalent to BIN_DIR semantics
  /// from the install script.
  #[arg(long)]
  bin_dir: Option<PathBuf>,
  ```
- [ ] Task B5. Thread the value into `update_cmd`
  (`crates/fono/src/cli.rs:1248-...`): when set, override
  `ApplyOpts.target_override` with `bin_dir.join("fono")`. Confirm
  the override survives the `is_package_managed` check (i.e. if
  the user explicitly points `--bin-dir /usr/bin`, we still refuse;
  override is for `BIN_DIR` style paths like `/opt/fono/bin` or
  `~/.local/bin`).
- [ ] Task B6. Integration test in
  `crates/fono-update/tests/apply_update_dry_run.rs` (or extend an
  existing test file) that passes `target_override` to a temp dir
  and asserts the dry-run path resolves correctly. Use a bogus
  asset URL that returns a small fixture from a local
  `tokio::net::TcpListener` — or, simpler, factor `apply_update`
  so the download step accepts a pre-opened `&mut std::fs::File`
  and bypass the network entirely for the test.

#### Phase B3 — Release workflow per-asset `.sha256`

- [ ] Task B7. Edit `.github/workflows/release.yml:327-335` (the
  `Checksum` step). After producing `SHA256SUMS`, also write each
  individual `<asset>.sha256` file: a one-line `<hex>  <asset>`.
  Both the aggregate `SHA256SUMS` and the per-asset `.sha256`
  files become release assets. Adjust `softprops/action-gh-release`
  glob (`files: release/*` already covers it).
- [ ] Task B8. Confirm the change doesn't blow up if no
  `MINISIGN_KEY` is set — the existing minisign step is gated; the
  per-asset `.sha256` files are unsigned by design. The guarantee
  is "if you trust GitHub TLS to deliver the right `.sha256`, you
  trust it to deliver the right binary"; the minisign step adds
  the offline-verifiable layer on top of that.

#### Phase B4 — `docs/dev/update-qa.md` checklist

- [ ] Task B9. Author `docs/dev/update-qa.md` covering:
  - Scenario 1: bare-binary install via the install one-liner →
    `fono update --check` reports up-to-date; `fono update -y`
    runs to completion and `fono --version` reports the new
    version after `execv`.
  - Scenario 2: `/usr/local/bin/fono` writable by current user →
    update succeeds.
  - Scenario 3: `/usr/local/bin/fono` owned by root → update
    refuses with `try sudo fono update` hint.
  - Scenario 4: distro-packaged `/usr/bin/fono` →
    `is_package_managed` refusal with distro-command hint.
  - Scenario 5: offline (`FONO_NO_UPDATE_CHECK=1`) → silent.
  - Scenario 6: rate-limited GitHub (mock by deleting cache and
    spamming requests) → `CheckFailed` reported, prior cache
    survives.
  - Scenario 7: mismatched `.sha256` sidecar → refusal with no
    rename; binary unchanged.
  - Scenario 8: prerelease channel selection.
  - Scenario 9: `--bin-dir` override.
  - Scenario 10: rollback by manually swapping `<exe>.bak` over
    `<exe>`.
- [ ] Task B10. Add a one-line cross-reference to `docs/dev/update-qa.md`
  at the top of `plans/2026-04-27-fono-self-update-v1.md` and tick
  Tasks 12, 16, 20, 22 of that plan. Update its `## Status` header
  to `Status: Landed in <commit-B>; full plan delivered modulo
  the smoke-test --self-check flag (Task 15) which remains
  intentionally deferred per ADR 0009 §5`.

### Thread C — Real-fixture CI bench gate

#### Phase C1 — Bake the baseline JSON

- [ ] Task C1. Locally run
  `cargo run -p fono-bench --features equivalence,whisper-local --
  equivalence --stt local --model tiny.en --output
  docs/bench/baseline-comfortable-tiny-en.json --no-legend`
  against the committed manifest. Whisper `tiny.en` model must be
  present at `~/.cache/fono/models/whisper/ggml-tiny.en.bin`; if
  absent, run `fono models install whisper-tiny.en` first or
  download via `fono-download` directly.
- [ ] Task C2. Inspect the produced JSON. Expected: 4 English
  fixtures with `Pass` (or `Fail` if the existing thresholds turn
  out too tight — we ship `Pass`-able thresholds; if a fixture
  fails locally, that's a real regression from the doc-recon-pass
  state and stops this thread). 6 non-English fixtures
  `SkipReason::Capability`. `model_capabilities` block populated.
  `pinned_params` block populated.
- [ ] Task C3. Strip the JSON of timing fields that vary per run
  (`elapsed_ms`, `ttff_ms`, `duration_s` for skipped rows): use a
  small post-processing pass — `jq` or a `cargo run -p fono-bench
  -- equivalence-baseline-strip` subcommand if jq isn't reliably
  available. **Recommendation:** add a `--baseline` flag to
  `fono-bench equivalence` that, after running, writes a
  deterministic subset of the report (every field except absolute
  timings; ratios are kept since they're relative). Avoids a `jq`
  dep on CI.
- [ ] Task C4. Commit
  `docs/bench/baseline-comfortable-tiny-en.json` (the stripped
  version). Add `docs/bench/README.md` (~30 lines) explaining how
  to regenerate the baseline, when to update it, and how the CI
  comparison step interprets it.

#### Phase C2 — CI gate

- [ ] Task C5. Replace the compile-only `cargo bench --no-run`
  step at `.github/workflows/ci.yml:64-68` with two new steps:
  - **Step 1 — fetch the `tiny.en` whisper model.** Add to the
    job's existing `steps:` block:
    ```yaml
    - name: Download whisper tiny.en model (cached)
      shell: bash
      run: |
        set -euo pipefail
        cache_dir="${HOME}/.cache/fono/models/whisper"
        mkdir -p "${cache_dir}"
        if [[ ! -s "${cache_dir}/ggml-tiny.en.bin" ]]; then
          curl -fsSL --retry 3 \
            -o "${cache_dir}/ggml-tiny.en.bin" \
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin"
        fi
        # Verify SHA-256 against the value in the model registry
        # so a poisoned mirror can't sneak past CI.
        echo "<EXPECTED_SHA>  ${cache_dir}/ggml-tiny.en.bin" \
          | sha256sum --check -
    ```
    Substitute `<EXPECTED_SHA>` with the actual digest from the
    `fono-stt` `ModelRegistry`. Use the `actions/cache@v4` action
    on `~/.cache/fono/models/whisper` keyed on the model SHA so
    repeat runs skip the download.
  - **Step 2 — equivalence harness gate.**
    ```yaml
    - name: fono-bench equivalence (tiny.en gate)
      run: |
        cargo run --release -p fono-bench \
          --features equivalence,whisper-local -- \
          equivalence \
            --stt local \
            --model tiny.en \
            --output ci-bench.json \
            --baseline \
            --no-legend
        # Compare structural verdicts against the committed baseline.
        python3 - <<'PY'
        import json, sys
        ours = json.load(open('ci-bench.json'))
        gold = json.load(open('docs/bench/baseline-comfortable-tiny-en.json'))
        # Per-fixture verdicts must match exactly. Timings are
        # not compared (the baseline file omits them).
        diffs = []
        ours_by = {r['fixture']: r['verdict'] for r in ours['results']}
        gold_by = {r['fixture']: r['verdict'] for r in gold['results']}
        for k in sorted(set(ours_by) | set(gold_by)):
            if ours_by.get(k) != gold_by.get(k):
                diffs.append(f"{k}: ci={ours_by.get(k)} baseline={gold_by.get(k)}")
        if diffs:
            print("equivalence verdicts diverged from baseline:")
            for d in diffs: print(f"  {d}")
            sys.exit(1)
        print(f"OK — {len(ours_by)} fixture verdicts match baseline.")
        PY
    ```
- [ ] Task C6. Keep the existing `cargo test -p fono-bench --release
  -- --ignored --nocapture` latency-smoke step
  (`ci.yml:64-65`) — the equivalence gate does not subsume it; the
  ignored tests cover different code paths (`fono-bench` runner
  benches, not the equivalence harness).
- [ ] Task C7. Document the new gate in
  `docs/plans/2026-04-25-fono-roadmap-v2.md`: tick R5.1 (now real),
  tick R5.2 (baseline JSON committed). Update its summary line.

#### Phase C3 — Plan checkbox close-out

- [ ] Task C8. Update `docs/status.md` "Recommended next session"
  block to point at **Wave 3** (Slice B1 — realtime cpal push +
  Groq streaming) instead of Wave 2.

### Phase D — Verification + commit

- [ ] Task D1. Re-run the full gate: `cargo build --workspace`,
  `cargo test --workspace --all-targets`, `cargo clippy --workspace
  --all-targets -- -D warnings`. All three must pass cleanly.
- [ ] Task D2. Run `cargo run -p fono-bench --features
  equivalence,whisper-local -- equivalence --stt local --model
  tiny.en --output /tmp/local.json --no-legend` end-to-end. Diff
  `/tmp/local.json` (after `--baseline` stripping) against the
  committed `docs/bench/baseline-comfortable-tiny-en.json`. They
  must match — that's the contract CI will enforce.
- [ ] Task D3. Run `tests/bench.sh tiny.en` if the script is
  executable. Confirm output matches expectations: 4 English
  fixtures pass, 6 non-English skip with capability-induced reason.
- [ ] Task D4. `git status` review. Expected modified / new files:
  - **Thread A:**
    - `crates/fono-bench/src/capabilities.rs` (new).
    - `crates/fono-bench/src/lib.rs` (added `pub mod capabilities;`).
    - `crates/fono-bench/src/equivalence.rs` (manifest schema +
      `run_fixture` signature + `decide_verdict` signature +
      `EquivalenceReport.model_capabilities` + `SkipReason`).
    - `crates/fono-bench/src/bin/fono-bench.rs` (capability
      resolution moved out of the inline boolean; passes `&caps`
      into `run_fixture`).
    - `tests/fixtures/equivalence/manifest.toml` (renames + new
      `accuracy_threshold` per fixture).
    - `crates/fono-bench/tests/capability_skip.rs` (new).
    - `crates/fono-bench/tests/two_gate_verdict.rs` (new).
    - `plans/2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md` (status + checkboxes).
  - **Thread B:**
    - `crates/fono-update/src/lib.rs` (`UpdateInfo` fields +
      `apply_update` sidecar verification + `parse_sha256_sidecar`).
    - `crates/fono/src/cli.rs` (`--bin-dir` flag + plumb).
    - `crates/fono-update/tests/apply_update_dry_run.rs` (new or
      extended).
    - `.github/workflows/release.yml` (per-asset `.sha256`).
    - `docs/dev/update-qa.md` (new).
    - `plans/2026-04-27-fono-self-update-v1.md` (status + checkboxes).
  - **Thread C:**
    - `crates/fono-bench/src/bin/fono-bench.rs` (`--baseline` flag).
    - `crates/fono-bench/src/equivalence.rs` (deterministic-output
      helper for `--baseline`).
    - `docs/bench/baseline-comfortable-tiny-en.json` (new).
    - `docs/bench/README.md` (new).
    - `.github/workflows/ci.yml` (model-cache step + equivalence
      gate step).
    - `docs/plans/2026-04-25-fono-roadmap-v2.md` (R5.1 / R5.2 ticks).
    - `docs/status.md` (Recommended next session → Wave 3).
- [ ] Task D5. Stage and commit in three DCO-signed chunks:
  1. `feat(fono-bench): typed ModelCapabilities + split equivalence/accuracy thresholds`
     — Thread A files only.
  2. `feat(fono-update): per-asset .sha256 sidecar verification + --bin-dir flag`
     — Thread B files only.
  3. `ci: real-fixture equivalence gate with tiny.en + baseline JSON anchor`
     — Thread C files only.
  Each commit message body cites the originating plan
  (`plans/2026-04-28-doc-reconciliation-v1.md` Wave 2 reference,
  the half-shipped plan being closed, the file:line evidence for
  the hot path of the change).

- [ ] Task D6. Final summary report listing the three commit SHAs,
  the test/clippy/build outputs at each stage, and the equivalence
  baseline-vs-CI diff result. Note any deviations from the plan
  taken during execution and the rationale.

## Verification Criteria

- `cargo build --workspace`, `cargo test --workspace --all-targets`,
  `cargo clippy --workspace --all-targets -- -D warnings` all green
  before and after the wave (Tasks 0, D1).
- `crates/fono-bench/src/capabilities.rs` exists with `ModelCapabilities`,
  `for_local_whisper`, `for_cloud`, `fixture_requires_multilingual`,
  and unit tests for the quantization-stem normalisation, English-only
  classification, cloud multilingual default, and override behaviour
  (Tasks A1-A3).
- `tests/fixtures/equivalence/manifest.toml` parses cleanly under
  the new schema (alias from `levenshtein_threshold` →
  `equivalence_threshold` works), and every fixture carries either
  an explicit `accuracy_threshold` or relies on the documented
  fallback (Task A5). Each existing fixture's verdict is unchanged
  vs the pre-wave behaviour (Task D2).
- `EquivalenceReport` JSON contains a populated
  `model_capabilities` block with `english_only` and `model_label`
  (Task A10). Skipped rows carry a populated `skip_reason` field
  (Task A11).
- A new `cargo test -p fono-bench --test capability_skip` passes;
  the test asserts `transcribe` is never invoked (uses a `PanicStt`
  mock) and that the verdict is `Skipped` with reason `Capability`
  (Task A13). A sibling `two_gate_verdict` integration test asserts
  `Verdict::Fail` when accuracy diverges (Task A14).
- `fono update --bin-dir <path>` resolves to that directory and
  honours the `is_package_managed` rejection on system paths
  (Tasks B4-B5). A new dry-run integration test passes (Task B6).
- `apply_update` rejects a downloaded asset whose SHA-256 does not
  match the published `.sha256` sidecar, leaves the original
  binary untouched (Task B2). When no sidecar is published (legacy
  release), proceeds with a `warn!` and trusts TLS (back-compat
  with `v0.1.x` and `v0.2.x` releases).
- `parse_sha256_sidecar` correctly handles the four canonical
  sidecar shapes (Task B3).
- `.github/workflows/release.yml` produces a `<asset>.sha256` file
  per artefact in addition to `SHA256SUMS` (Task B7).
- `docs/dev/update-qa.md` exists and lists the ten scenarios above
  with explicit pass / fail criteria for each (Task B9).
- `.github/workflows/ci.yml` runs `fono-bench equivalence --stt
  local --model tiny.en --baseline` on every PR; the step fails
  the build when any per-fixture verdict diverges from
  `docs/bench/baseline-comfortable-tiny-en.json` (Tasks C1-C5).
  The whisper `tiny.en` model is cached across runs via
  `actions/cache` keyed on the model SHA (Task C5).
- `docs/bench/baseline-comfortable-tiny-en.json` is committed,
  contains the deterministic subset of the harness output (no
  absolute timings) for the 10 fixtures (4 Pass, 6 Skipped), and
  is regenerable via `cargo run -p fono-bench -- equivalence
  --baseline`.
- `docs/bench/README.md` documents how to regenerate the baseline,
  when to update it, and the CI gate's contract.
- `plans/2026-04-27-fono-self-update-v1.md` and
  `plans/2026-04-28-equivalence-harness-language-gating-and-accuracy-v1.md`
  show every plan task ticked (or annotated with an explicit
  deferred-with-rationale tag for the smoke `--self-check` flag
  intentionally left for a later wave).
- `docs/plans/2026-04-25-fono-roadmap-v2.md` Tier-1 R5.1 and R5.2
  are ticked (Task C7).
- `docs/status.md` "Recommended next session" block points at
  Wave 3 (Slice B1) (Task C8).
- Three DCO-signed commits land on `main`, one per thread (Task D5).

## Potential Risks and Mitigations

1. **CI tiny.en model download flakes / HuggingFace rate-limits.**
   Mitigation: `actions/cache@v4` keyed on the model SHA so cold
   downloads happen at most once per cache eviction; SHA-256
   verification step catches a poisoned cache; `curl --retry 3`
   in the download command handles transient 503s.

2. **`tiny.en` baseline drifts subtly between runs because of
   non-determinism in whisper.cpp's beam-search.** The 4 English
   fixtures are short and currently produce stable text; if any
   one of them flaps between `Pass` and `Fail` on CI, the whole
   gate becomes a flake-source.
   Mitigation: the baseline compares **verdicts** (Pass / Fail /
   Skipped), not raw transcripts. The two-gate threshold is loose
   enough (0.20) to absorb minor punctuation drift. If a fixture
   flaps despite that, demote its threshold to `1.0`
   (informational-only) the same way `en-single-sentence` and
   `zh-luxun-kuangren` are handled today, and document the flap
   in `docs/bench/README.md`.

3. **`serde(alias = "levenshtein_threshold")` collision with the
   field rename in Task A4** if both the alias and the canonical
   name appear in the same TOML.
   Mitigation: the manifest never carries both — Task A5 renames
   every occurrence. Add a one-line test that asserts a fixture
   carrying both `levenshtein_threshold = 0.05` and
   `equivalence_threshold = 0.10` in the same entry produces a
   parse error (or, if serde silently picks one, document which).
   The plan-level expectation is "alias is read-only; canonical is
   write-only".

4. **`--baseline` flag in `fono-bench` may collide with an existing
   flag of the same name.** Verify with
   `crates/fono-bench/src/bin/fono-bench.rs` clap definition.
   Mitigation: search for `--baseline` before adding; rename to
   `--strip-timings-for-baseline` if a collision exists. (The
   strategic plan recommendation prefers the short name when
   available.)

5. **`SkipReason` enum addition is a breaking change for any
   external consumer of `EquivalenceResult` JSON.**
   Mitigation: `#[serde(default)]` on the new field so older
   reports without it parse cleanly into the new shape (with
   `skip_reason: None`); never write a `Skipped` row with
   `skip_reason: None` going forward — the field is required for
   new rows.

6. **`apply_update` `.sha256` sidecar fetch slows the update path
   meaningfully.** A second HTTPS round-trip on each update.
   Mitigation: pre-fetch in `fetch_latest` (Task B1) so the
   user-perceived `apply_update` runtime is unchanged; the
   pre-fetch is < 100 bytes and rides on the same TLS connection
   keepalive as the asset metadata.

7. **The `--bin-dir` flag conflicts with the FSM-controlled
   `target_override` already used by tests.**
   Mitigation: CLI maps `--bin-dir <p>` → `ApplyOpts.target_override
   = Some(p.join("fono"))`. `target_override` remains the internal
   API; `--bin-dir` is the user-facing surface. Keep them
   coordinated via a single private constructor on `ApplyOpts`.

8. **Per-asset `.sha256` files multiply the release artefact count
   from ~5 to ~10**, possibly tripping `softprops/action-gh-release`
   asset upload limits or release page UX.
   Mitigation: GitHub releases support hundreds of assets; no
   limit at our count. Display order is alphabetical, so the
   `.sha256` files cluster next to their parent assets visually.
   If the UX feels noisy, ship only the aggregate `SHA256SUMS` and
   defer per-asset sidecars to a follow-up; in that case Task B2
   parses the aggregate `SHA256SUMS` instead of a sibling file.

9. **Mock-STT capability-skip test (Task A13) requires `PanicStt`
   to implement the full `SpeechToText` trait** including
   `transcribe`, `prewarm`, `name`, `supports_streaming`. Trait
   surface is large.
   Mitigation: declare only the required methods; `unimplemented!()`
   the rest. Alternative: wrap a no-op stub and assert the panic
   path indirectly. The simpler, more honest approach is to put
   `panic!("must not be invoked")` in `transcribe` and **not
   provide** the other methods — the trait must be fully implemented,
   so use `unimplemented!("not used in capability-skip test")` for
   non-`transcribe` calls. The test only invokes `transcribe`
   indirectly through `run_fixture`, which short-circuits before
   reaching it.

10. **Local baseline (Task C1) needs the whisper `tiny.en` GGML
    model on the developer machine.** Not every contributor will
    have it cached.
    Mitigation: `docs/bench/README.md` documents the regeneration
    procedure; the CI workflow downloads the model and would
    produce the baseline equivalently. For first-time setup,
    document `cargo run -p fono -- models install whisper-tiny.en`.

## Alternative Approaches

1. **Skip Thread A (the typed `ModelCapabilities` refactor) and
   ship only Threads B and C.** The current inline boolean and the
   conflated single threshold both work today. Trade-off: the
   strategic plan's leverage analysis named A first; deferring it
   means Wave 3 / Wave 4 cloud streaming work bumps into the
   rough surface again. Rejected — A is one focused commit.

2. **Run the equivalence gate against `whisper-small`
   (multilingual) instead of `tiny.en`.** Trade-off: covers all
   10 fixtures with real inference (no capability skips) and
   matches the strategic-plan target tier (Comfortable). But CPU
   minutes per PR roughly 4-5x; Romanian / Chinese fixtures are
   already informational-only because of streaming-mojibake; gain
   is marginal. Rejected for the per-PR gate; recommend it for a
   nightly scheduled CI job in a follow-up.

3. **Combine Threads B and C into one commit ("CI hardening").**
   Trade-off: smaller `git log`, but mixes self-update server-side
   work (`release.yml`) with PR-side work (`ci.yml`); reverting one
   without the other becomes awkward. Rejected — three commits is
   the right granularity.

4. **Defer the `docs/dev/update-qa.md` checklist (Task B9).**
   Trade-off: lighter commit, but leaves the self-update plan with
   an explicit unticked task and no manual-verification anchor for
   future updater changes. Rejected — the checklist is half a
   page of prose and the plan stays cleaner with it landed.

5. **Drop `SkipReason` typing (Tasks A11-A12) and keep using note
   substring matching in `overall_verdict`.** Trade-off: less
   churn, but locks in stringly-typed coupling that the next
   feature (cloud streaming capability differences) will have to
   undo. Rejected — the typed enum is small and pays back in
   Wave 3.

6. **Skip the `.sha256` sidecar verification (Tasks B1-B3) and
   ship only the `--bin-dir` flag and the QA checklist.** Trade-off:
   the supply-chain hardening was a stated Wave 2 goal; ducking it
   keeps `apply_update` trusting TLS-to-GitHub alone, which is fine
   for now but means Wave 7 (full self-update polish) inherits the
   work instead. Rejected — it's the highest-impact part of
   Thread B for the diff size.
