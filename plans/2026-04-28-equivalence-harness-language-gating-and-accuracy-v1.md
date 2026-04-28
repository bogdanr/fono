# Equivalence Harness: Language Gating + Accuracy Check

## Status

~50% landed in commits `b6596c0` and `7db29b5` (2026-04-28) as inline
behaviour rather than a typed `ModelCapabilities` API. Remaining
typed-API refactor and combined-verdict work tracked as Wave 2 Task 7
of `plans/2026-04-28-doc-reconciliation-v1.md`.

## Objective

Stop wasting inference on fixtures the loaded model cannot possibly transcribe (e.g. running `tiny.en` over Spanish/French/Chinese/Romanian audio), and start producing a real accuracy verdict for the fixtures the model *is* expected to handle. The harness must keep its existing streaming↔batch equivalence gate intact and add a second, independent gate that compares the batch transcript against the manifest's `reference` text. End-state: when you run `./tests/bench.sh tiny.en`, the four English fixtures run normally and the six non-English fixtures show `SKIP — model is English-only`; when you run `./tests/bench.sh small`, every fixture runs and the report shows both an `equiv` (stream↔batch) verdict and an `acc` (batch↔reference) verdict.

## Background

- The verdict in `crates/fono-bench/src/equivalence.rs:458-482` is computed solely from `levenshtein_norm(stream_text, batch_text)`. Reference text is stored on `ManifestFixture::reference` (`crates/fono-bench/src/equivalence.rs:51`) but never consulted, which is why `tiny.en` "passes" Spanish: both lanes hallucinate identically.
- All non-English fixtures already carry `language = "es" | "fr" | "zh" | "ro"` in `tests/fixtures/equivalence/manifest.toml:74,92,110,135,154,167`; the manifest already encodes the ground truth.
- Whisper model capability follows a hard naming convention: any GGML model whose stem ends in `.en` is English-only. All other local Whisper variants (`tiny`, `base`, `small`, `medium`, `large-v3`, `large-v3-turbo`, …) are multilingual. Cloud Whisper endpoints (`groq` whisper-large-v3, `openai` whisper-1) are multilingual.
- `run_fixture` is the single entry point the CLI calls per row (`crates/fono-bench/src/bin/fono-bench.rs:355`), so a capability filter installed at or just before that call site catches every code path.

## Implementation Plan

### Phase 1 — Capability model

- [ ] Task 1. Introduce a `ModelCapabilities` value type in `fono-bench` (likely in a new `crates/fono-bench/src/capabilities.rs` re-exported from the crate root) carrying at minimum `english_only: bool` and a human-readable `model_label: String`. Keep the type plain-data so it can serialize into the JSON report later. Rationale: the harness needs a single, testable place to translate a backend identifier into "what languages can this model produce".
- [ ] Task 2. Implement a resolver `ModelCapabilities::for_local_whisper(model_stem: &str)` that returns `english_only = stem.ends_with(".en")` and `model_label = format!("local:{stem}")`. Add a sibling `for_cloud(provider, model)` that always returns `english_only = false` for the supported cloud providers (groq, openai). Document in the doc-comment that any future `.en`-only cloud SKU has to be added explicitly. Rationale: the only real-world capability axis we need to gate on today is English-only vs multilingual; richer per-language allow-lists can be added later without breaking the API.
- [ ] Task 3. Unit-test the resolver: `tiny.en` and `small.en` → english-only; `tiny`, `small`, `large-v3-turbo` → multilingual; cloud groq/openai → multilingual. Rationale: the rule is simple but trivially regress-able if someone adds a non-`.en` English-only model later, so the test set documents the contract.

### Phase 2 — Manifest schema for accuracy

- [ ] Task 4. Extend `ManifestFixture` (`crates/fono-bench/src/equivalence.rs:40-66`) with two new optional fields: `accuracy_threshold: Option<f32>` (per-fixture override for the batch↔reference gate) and `requires_multilingual: Option<bool>` (defaults to `language != "en"`). Rationale: keeping the "must be multilingual" decision as a derived default means the existing manifest doesn't need to grow, but an explicit override is available for edge cases (e.g. a future English fixture whose reference is annotated text that even multilingual models pass and `.en` doesn't).
- [ ] Task 5. Rename the existing `levenshtein_threshold` field semantically to "equivalence threshold" in docs/comments, but keep the wire name `levenshtein_threshold` for backwards compatibility with the committed manifest. Add a sibling `equivalence_threshold` alias via `#[serde(alias = "levenshtein_threshold")]` so future fixtures can use the clearer name. Rationale: avoids touching the 10 existing fixture entries while making the two-gate model legible.
- [ ] Task 6. Decide a sensible default `accuracy_threshold` constant — propose `0.20` for English fixtures (whisper-small / large produce near-verbatim) and document that non-English fixtures must set their own. In the manifest, populate `accuracy_threshold` per fixture: `0.10` for the four English clips, `0.30` for `es-lorca-reyerta` and `fr-gide-symphonie`, `0.50` for `zh-luxun-kuangren` (still informational while CJK streaming is broken), and `0.30` for the three Romanian fixtures. Rationale: ground-truth thresholds need to live next to the audio that motivates them so they can be tightened independently per language when models improve.

### Phase 3 — Two-gate verdict in `run_fixture`

- [x] Task 7. Change `run_fixture` (partial — implemented as inline boolean `english_only = args.stt == "local" && args.model.ends_with(".en")` at `crates/fono-bench/src/bin/fono-bench.rs:339`, not as a typed `ModelCapabilities`; `Verdict::Skipped` shape with note matches plan intent) (`crates/fono-bench/src/equivalence.rs:409-506`) to accept the resolved `ModelCapabilities` (or pass it through `EquivalenceConfig` if a config struct is already plumbed) and short-circuit before any `stt.transcribe` / `stream_transcribe` call when `caps.english_only && fx.requires_multilingual`. Return `Verdict::Skipped` with note `"model is English-only; fixture requires multilingual"`. Rationale: the user's hard requirement is "don't run inference on models that are supposed to fail" — the skip has to happen *before* any encoder pass, not just before the verdict.
- [x] Task 8. After the existing batch pass, compute `accuracy = levenshtein_norm(&batch.text, &fx.reference)` whenever `fx.reference` is non-empty. Store it on `Metrics` as a new `stt_accuracy_levenshtein: Option<f32>` (evidence: `crates/fono-bench/src/equivalence.rs:113-114`, populated at `:527`). Rationale: surface the number even when no threshold is configured so reports always show "how close was the model to the canonical text".
- [ ] Task 9. Combine the two gates into the verdict: `Pass` requires (a) the existing equivalence gate to pass *and* (b) when an accuracy threshold is in scope, `accuracy ≤ accuracy_threshold`. If the streaming pass is skipped (no streaming runtime available) but the accuracy check still produced a number, the verdict should reflect the accuracy result alone rather than today's blanket `Skipped`. Rationale: today the harness collapses to `Skipped` whenever streaming isn't wired; with a real reference comparison we have a meaningful answer even without streaming.
- [ ] Task 10. Extend `EquivalenceResult::note` to include both per-gate sub-verdicts when relevant (`equiv pass, acc fail (0.42 > 0.30)` style). Rationale: a single boolean verdict is too coarse once we have two independent gates; the operator needs to know which gate failed.

### Phase 4 — CLI wiring and reporting

- [ ] Task 11. In `crates/fono-bench/src/bin/fono-bench.rs:255-415`, resolve capabilities once after the STT is built (`Arc::new(WhisperLocal::new(path))` site at line 291; cloud arms once added) and thread the value into the `run_fixture` calls at line 355. Rationale: capabilities are a property of the loaded model, not of each fixture, so resolving once avoids re-deriving them per row.
- [x] Task 12. Add an `acc` column to the printed table (evidence: `crates/fono-bench/src/bin/fono-bench.rs:527`) (`print_table` at `crates/fono-bench/src/bin/fono-bench.rs:419-511`) showing the accuracy Levenshtein with the same green/yellow/red colour bands `fmt_lev` already provides. Update the legend block to describe the two gates. Rationale: the headline value the user wants ("how accurate is the model on this fixture") needs to be visible without piping JSON.
- [ ] Task 13. Persist the resolved capabilities into `EquivalenceReport` (new optional field, e.g. `model_capabilities: Option<ModelCapabilities>`) so downstream tools (`tests/bench.sh`, CI dashboards) can tell whether a `SKIP` row is operator-induced or capability-induced. Rationale: closes the audit loop — anyone reading the JSON should see why a fixture was skipped.
- [ ] Task 14. Update `EquivalenceReport::overall_verdict` (`crates/fono-bench/src/equivalence.rs:152-172`) so capability-induced skips never count toward `Skipped`-only outcomes — i.e., a run that skipped six rows due to English-only capabilities and passed four English rows must still report `Pass`, not `Skipped`. Rationale: the user explicitly wants `tiny.en` to be a *valid* run that just exercises a smaller subset.

### Phase 5 — Test, docs, regression coverage

- [ ] Task 15. Add unit tests in `crates/fono-bench/src/equivalence.rs` (alongside the existing `tests` module): one verifying capability-induced skip path returns `Skipped` with the right note and zero inference calls (use a mock STT that panics on `transcribe` to prove non-execution); one verifying the two-gate verdict (`equiv pass + acc fail` → `Fail`, `equiv pass + acc pass` → `Pass`, `equiv pass + no reference` → `Pass`); one verifying overall verdict treats capability-induced skips as inert. Rationale: locks in the contract the user is asking for so refactors don't silently regress it.
- [ ] Task 16. Add an integration smoke test (gated on `cfg(feature = "whisper-local")` and a present `tiny.en` model in cache) that runs the harness against the manifest and asserts: every `language != "en"` fixture is skipped with reason `model is English-only`, every English fixture runs both lanes. Rationale: end-to-end proof at the level the user invokes the tool.
- [x] Task 17. Update `tests/bench.sh` legend output (and the script's leading comment block) to mention the new `acc` column and the capability-skip behaviour. Multilingual fixtures shipped in tree (commit `b6596c0`). Rationale: the wrapper script is the canonical user entry point; its docs should match the new harness output.
- [x] Task 18. Update `docs/status.md` with the phase outcome and any follow-ups (e.g. tighten `acc` thresholds once `whisper-small` mojibake is fixed). Done in `plans/2026-04-28-doc-reconciliation-v1.md` Task 3 entry on `docs/status.md`.

## Verification Criteria

- Running `./tests/bench.sh tiny.en` exits 0, prints results for `en-single-sentence`, `en-multi-sentence`, `en-narrative-pause`, `en-conversational` only, and prints all six non-English fixtures as `SKIP` with note `"model is English-only"`. No whisper inference runs against any non-English WAV (verifiable by total wallclock time being roughly the sum of the four English fixtures, and by the unit test in Task 15).
- Running `./tests/bench.sh small` exits 0 when each fixture's batch transcript is within its configured `accuracy_threshold` of the manifest reference and within its configured equivalence threshold of the streaming text; non-zero exit when either gate trips, with the failing gate identified in the printed `note` column.
- The JSON report (`--output …`) contains, per fixture, `metrics.stt_accuracy_levenshtein` populated whenever `reference` is non-empty, plus a top-level `model_capabilities` block reflecting the loaded model.
- `cargo test -p fono-bench --features equivalence,whisper-local` passes, including the new tests in Tasks 15 and 16.
- Existing `tests/fixtures/equivalence/manifest.toml` continues to parse without modification (back-compat alias from Task 5 holds).

## Potential Risks and Mitigations

1. **`.en` suffix isn't a complete capability classifier.** Some downloaded GGML files may carry vendor-specific suffixes (e.g. `tiny.en-q5_1.bin`) that still strip out multilingual layers. The capability resolver must handle both the bare stem and any quantization suffix.
   Mitigation: in Task 2, normalize the stem by trimming a trailing `-q\d+(_\d+)?` quantization fragment before matching `.en`; cover both shapes in the unit tests.
2. **Accuracy threshold tuning is empirical.** The numbers chosen in Task 6 are best-effort; the first real run on `whisper-small` may show a fixture sitting just above its threshold, breaking CI.
   Mitigation: land Task 6's thresholds with the same `informational only` doc-comment pattern already used for `zh-luxun-kuangren` (`levenshtein_threshold = 1.0`), and tighten them in a follow-up commit only after observing stable numbers across two distinct runs.
3. **Reference text formatting drift.** The references in the manifest are lower-cased prose; if a future fixture is added with raw mixed-case punctuation, `levenshtein_norm`'s normalization (case-fold + whitespace collapse) won't strip punctuation, inflating the distance.
   Mitigation: document the expected manifest reference shape (lowercase, ASCII punctuation only where unavoidable, no leading/trailing whitespace) in the new doc-comment on `ManifestFixture::reference`; consider an optional, opt-in punctuation-stripping mode in a later iteration if the corpus grows.
4. **Cloud SKUs change capability silently.** OpenAI or Groq could ship an English-only Whisper variant in the future; today's `for_cloud` returns multilingual for everything.
   Mitigation: keep the resolver explicit (per-provider match arms) so adding a new SKU forces a code change, and surface unknown SKUs as a warning rather than silent multilingual.
5. **Hidden coupling with the legacy `bench` subcommand.** The plan touches `equivalence.rs` and the equivalence CLI arm, but `runner::BenchRunner` (`crates/fono-bench/src/bin/fono-bench.rs:183-253`) also iterates fixtures and could regress if it shares the manifest type.
   Mitigation: search the workspace for users of `ManifestFixture` and `Manifest` before merging Task 4; if the legacy bench runner consumes them, mirror the new fields with `#[serde(default)]` so older code paths continue to compile and ignore the additions.

## Open follow-ups (carried into Wave 2 Task 7)

- Tasks 1–6 — typed `ModelCapabilities` value type in a new
  `crates/fono-bench/src/capabilities.rs`, `for_local_whisper` /
  `for_cloud` resolvers + unit tests, `accuracy_threshold` and
  `requires_multilingual` fields on `ManifestFixture`,
  `equivalence_threshold` serde alias, per-fixture default thresholds.
- Tasks 9–10 — combined two-gate verdict in `run_fixture` (Pass requires
  equivalence pass *and* accuracy within threshold) + per-gate
  sub-verdicts in `EquivalenceResult::note`.
- Task 11 — capabilities resolved once after STT build and threaded
  through `run_fixture` (today re-derived inline at the call site).
- Task 13 — `EquivalenceReport.model_capabilities` field for downstream
  tooling.
- Task 14 — `EquivalenceReport::overall_verdict` treats capability
  skips as inert by typed contract (incidentally true today as inline
  behaviour).
- Task 15 — mock-STT capability-skip unit test + two-gate verdict
  unit tests (Pass / Fail / no-reference cases) + overall-verdict
  inertness test.
- Task 16 — integration smoke test gated on `cfg(feature = "whisper-local")`
  and a present `tiny.en` model.

## Alternative Approaches

1. **Manifest-driven `requires_multilingual` only, no capability resolver.** Skip Phase 1 and instead let every non-English fixture explicitly set a list of acceptable model regexes. Trade-off: pushes the policy into 6+ manifest entries instead of one resolver, easier to forget when adding a new fixture, but avoids any code-side enumeration of model SKUs.
2. **Hard-fail rather than skip on capability mismatch.** Treat "English-only model + multilingual fixture" as `Fail` instead of `Skipped`. Trade-off: matches the spirit of "tiny.en should not pass Spanish" more aggressively but breaks the user's stated requirement of *not* running these inferences for tiny.en runs at all and would force CI to special-case tiny.en runs.
3. **Drop streaming↔batch equivalence entirely and gate only on accuracy.** Trade-off: simpler model, but loses the original Slice A R18 mandate; the equivalence gate exists to catch streaming-lane regressions even when accuracy stays acceptable. Recommended only as a last resort if the two-gate model proves unmaintainable.
4. **Compute accuracy outside the harness via a separate `fono-bench accuracy` subcommand.** Trade-off: cleaner separation of concerns and matches the existing legacy `bench` subcommand split, but doubles wallclock time because the audio would be transcribed twice (once for equivalence, once for accuracy) unless an intermediate cache is added.
