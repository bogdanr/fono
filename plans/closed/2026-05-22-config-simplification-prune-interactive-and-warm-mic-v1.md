# Config Simplification — Prune Inert `[interactive]` Knobs and `always_warm_mic`

## Status: Completed

## Objective

Reduce `fono_core::config` surface area by removing nine fields that are either
inert at runtime (write-only), explicitly reserved-for-future, or expose
internal heuristic tuning that should not be user-facing. Also remove the
matching `Keep microphone always-on` toggle from the tray menu since
`always_warm_mic` does nothing.

Fields targeted for deletion from `crates/fono-core/src/config.rs`:

- `General.always_warm_mic`
- `Interactive.commit_use_prosody`
- `Interactive.commit_prosody_extend_ms`
- `Interactive.commit_use_punctuation_hint`
- `Interactive.commit_punct_extend_ms`
- `Interactive.commit_hold_on_filler`
- `Interactive.commit_filler_words`
- `Interactive.commit_dangling_words`
- `Interactive.eou_drain_extended_ms`
- `Interactive.eou_adaptive`
- `Interactive.resume_grace_ms`
- `Interactive.budget_ceiling_per_minute_umicros`
- `Interactive.max_session_seconds`
- `Interactive.max_session_cost_usd`

Out of scope: `chunk_ms_initial`, `chunk_ms_steady`, `mode`, `quality_floor`,
`streaming_interval`, `hold_release_grace_ms`, `cleanup_on_finalize`. (These
are either actually used or specifically requested to stay.)

## Background — what the deletions imply

- The boundary-heuristic defaults still need to live somewhere because
  `LiveSession` uses them internally via `crates/fono/src/live.rs:60-74`
  (`HeuristicConfig::default()`). We are **not** deleting `HeuristicConfig`,
  `default_filler_words()`, or `default_dangling_words()` — only the public
  config knobs that today fail to plumb into them. Defaults remain in code;
  the user just stops being able to override them in TOML.
- `Interactive.budget_ceiling_per_minute_umicros` plumbing is supplied by
  `crates/fono/src/live.rs:511 fn budget_for(...)`, which is never called.
  Deleting the config field lets us also delete that helper. The active
  `LiveSession::with_budget` path keeps `BudgetController::local()` (no cap)
  as the default — unchanged behaviour.
- `General.always_warm_mic` has a full tray-menu surface (`PreferencesSnapshot`,
  `TrayAction::SetAlwaysWarmMic`, the checkmark item, the daemon handler),
  but **no consumer** in `fono-audio` — it never gated any latency-plan-L1
  code. Removing the toggle is purely UX cleanup; no behaviour changes.
- This is a **schema breaking change** for any user who set these in
  `config.toml`. Mitigation strategy below uses serde's tolerant unknown-field
  behaviour (Fono's structs don't carry `deny_unknown_fields` except for
  `Network` and `ServerWyoming`, neither of which is touched), so old
  configs continue to load and just ignore the dropped keys. No `migrate`
  arm is strictly required, but we should still verify.

## Implementation Plan

### 1. Verify migration safety

- [ ] Task 1. Confirm that `Config`, `General`, and `Interactive` structs are **not** annotated with `#[serde(deny_unknown_fields)]`. Rationale: an old `config.toml` containing the now-removed keys must keep parsing — silently dropping unknown keys gives a frictionless upgrade. (Spot-check: `crates/fono-core/src/config.rs:14-76,117-119,1005-1008`.)
- [ ] Task 2. Decide whether to bump `CURRENT_VERSION`. Recommended: **no** bump, because there is no field whose semantics change — fields simply disappear. The migrate function's "version too new" guard at `crates/fono-core/src/config.rs:1237-1246` already handles forward-load attempts from older binaries.

### 2. Remove fields from `crates/fono-core/src/config.rs`

- [ ] Task 3. Delete `pub always_warm_mic: bool` from the `General` struct (`config.rs:141`) and its corresponding `always_warm_mic: false` line in `General::default()` (`config.rs:163`). Update the doc-comment block above `General` if it mentions warm-mic semantics.
- [ ] Task 4. Delete from the `Interactive` struct (`config.rs:1008-1112`): `budget_ceiling_per_minute_umicros`, `max_session_seconds`, `max_session_cost_usd`, `commit_use_prosody`, `commit_prosody_extend_ms`, `commit_use_punctuation_hint`, `commit_punct_extend_ms`, `commit_hold_on_filler`, `commit_filler_words`, `commit_dangling_words`, `eou_drain_extended_ms`, `eou_adaptive`, `resume_grace_ms`. Drop their doc-comments along with the fields.
- [ ] Task 5. Mirror the deletions in `Interactive::default()` at `config.rs:1115-1138`: remove the 13 lines initialising the deleted fields. Keep `quality_floor`, `mode`, `chunk_ms_initial`, `chunk_ms_steady`, `cleanup_on_finalize`, `streaming_interval`, `hold_release_grace_ms`.
- [ ] Task 6. Decide whether `default_filler_words()` (`config.rs:1173`) and `default_dangling_words()` (`config.rs:1179`) should stay in `fono_core::config` or move to `crates/fono/src/live.rs`. Recommended: **keep them where they are** as `pub fn` exports — `HeuristicConfig::default()` at `crates/fono/src/live.rs:68-69` already imports them from `fono_core::config`, and `fono-bench`'s equivalence harness uses the same canonical lists. Adjust the doc-comment to drop the mention of `commit_hold_on_filler`.
- [ ] Task 7. Re-evaluate whether the `#[allow(clippy::struct_excessive_bools)]` attribute on `Interactive` (`config.rs:1007`) is still needed after deletions — the remaining struct has only `cleanup_on_finalize` as a bool, so the attribute should be removed.
- [ ] Task 8. Re-evaluate the same attribute on `General` (`config.rs:119`) — without `always_warm_mic` it still has four bools (`startup_autostart`, `auto_mute_system`, `also_copy_to_clipboard`, `cloud_rerun_on_language_mismatch`), which still trips the lint. Keep the allow.

### 3. Remove tests covering the deleted fields

- [ ] Task 9. In `crates/fono-core/src/config.rs:1316-1386`, rewrite `interactive_v7_keys_round_trip` and `empty_interactive_block_yields_defaults` to assert only on the surviving fields (`quality_floor`, `mode`, `chunk_ms_initial`, `chunk_ms_steady`, `cleanup_on_finalize`, `streaming_interval`, `hold_release_grace_ms`). Delete the lines that set or assert the removed keys. Rename `interactive_v7_keys_round_trip` to something neutral (e.g. `interactive_keys_round_trip`) since the v7 vocabulary is gone.
- [ ] Task 10. Add a regression test `legacy_interactive_keys_are_ignored_silently` that constructs a TOML string containing several of the now-removed keys (e.g. `commit_use_prosody = true`, `budget_ceiling_per_minute_umicros = 1000`) and asserts `Config::load` succeeds without error. Protects existing user configs from breakage. Rationale: documents the unknown-field-tolerance assumption.

### 4. Remove `always_warm_mic` from the tray surface

- [ ] Task 11. In `crates/fono-tray/src/lib.rs`: delete `pub always_warm_mic: bool` from `PreferencesSnapshot` (line 158), delete the `SetAlwaysWarmMic(bool)` variant and its doc comment (lines 255-256), and delete the `prefs_check("Keep microphone always-on …", p.always_warm_mic, TrayAction::SetAlwaysWarmMic)` block in `build_preferences_submenu` (lines 1248-1252).
- [ ] Task 12. Adjust the comment block at `crates/fono-tray/src/lib.rs:148-153` if it specifically calls out the warm-mic toggle — otherwise leave it.
- [ ] Task 13. In `crates/fono/src/daemon.rs`: delete the `TrayAction::SetAlwaysWarmMic(v) => { … }` arm at lines 928-936, and delete the `always_warm_mic: cfg.general.always_warm_mic,` line at line 2024.

### 5. Remove the now-orphan `budget_for` helper

- [ ] Task 14. Delete `pub fn budget_for(...)` at `crates/fono/src/live.rs:511-514`. It is referenced only by the doc-comment on `Interactive.max_session_cost_usd` (which we are deleting in Task 4) and nowhere else in the workspace. Confirm by grepping for `budget_for` after deletion — only fallout should be in `plans/` / `docs/` (informational, see Task 17).

### 6. Audit fono-bench / equivalence harness

- [ ] Task 15. Inspect `crates/fono-bench/src/equivalence.rs:301-330` — that file has its **own** `commit_*` struct fields for the bench's `BoundaryConfig`. Those are independent of `fono_core::config::Interactive` (bench reads them from its own TOML, not from `config.toml`). **No change required** — leave the bench harness alone. Add a one-line comment in the bench struct's doc cross-referencing that these mirror `HeuristicConfig` from `fono/src/live.rs` rather than `fono_core::config`.

### 7. Documentation sync

- [ ] Task 16. Update `docs/interactive.md` to remove the now-defunct configuration knobs and explain that boundary heuristics ship with built-in defaults that are not user-tunable in this release.
- [ ] Task 17. Add a `## [X.Y.Z] — YYYY-MM-DD` section to `CHANGELOG.md` (release-time, per `AGENTS.md`) noting the breaking schema simplification, listing the removed keys, and stating that old configs continue to load — the keys are simply ignored.
- [ ] Task 18. Update `docs/status.md` end-of-session log with the simplification.
- [ ] Task 19. If `docs/providers.md` or any ADR (`docs/decisions/0015-boundary-heuristics.md` is a likely candidate) references the removed keys as user-tunable, add a note clarifying that as of this change they are internal defaults only.

### 8. Verify the pre-commit gate (AGENTS.md hard rule)

- [ ] Task 20. Run, in order: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --tests --lib`. Each must exit 0 before commit.
- [ ] Task 21. Run any integration tests touching live-dictation (`crates/fono/tests/live_pipeline.rs`, `crates/fono/tests/wizard_primary_flow.rs`, `crates/fono/tests/pipeline.rs`) to confirm the streaming pipeline still uses `HeuristicConfig::default()` end-to-end without regression.

## Verification Criteria

- An empty `config.toml` (or one omitting the `[interactive]` block) loads identically before and after the change.
- An old `config.toml` containing any of the 14 removed keys loads without error and without parser warnings; the removed keys are silently ignored.
- `Config::default()` produces a `Config` whose serialised TOML no longer contains any of the 14 removed keys.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace --tests --lib` all exit 0.
- The tray "Preferences" submenu no longer shows a "Keep microphone always-on" checkbox.
- `rg --type rust 'always_warm_mic|SetAlwaysWarmMic|commit_use_prosody|commit_prosody_extend|commit_use_punctuation|commit_punct_extend|commit_hold_on_filler|commit_filler_words|commit_dangling_words|eou_drain_extended_ms|eou_adaptive|resume_grace_ms|budget_ceiling_per_minute_umicros|max_session_seconds|max_session_cost_usd|budget_for' crates/` returns matches **only** in `crates/fono-bench/src/equivalence.rs` (the bench harness's own independent struct) and in `crates/fono/src/live.rs` for `HeuristicConfig`'s internal field names (`prosody_extend_ms`, `punct_extend_ms`, `hold_on_filler`, `filler_words`, `dangling_words`, `eou_drain_extended_ms` — these are the *internal* fields, not the config fields, and they stay).
- The live streaming pipeline produces identical `LiveTranscript.committed` strings against the fono-bench A2 row before and after the change (because `HeuristicConfig::default()` is unchanged).

## Potential Risks and Mitigations

1. **User upgrade pain — existing `config.toml` rejected by the new binary.**
   Mitigation: Verify `Config` / `General` / `Interactive` have no `deny_unknown_fields` (Task 1) and add `legacy_interactive_keys_are_ignored_silently` regression test (Task 10).

2. **Hidden consumer of `always_warm_mic` somewhere outside the searched paths
   (e.g. an `fono-audio` cpal-keep-alive branch added later).**
   Mitigation: The audit (Section 6 of the prior research) covered every `.rs` file under `crates/`. Re-run the grep in the verification criteria as a final gate.

3. **Default boundary-heuristic behaviour changes by accident if
   `HeuristicConfig::default()` drifts from the previous config defaults.**
   Mitigation: `HeuristicConfig::default()` at `crates/fono/src/live.rs:60-74` already encodes exactly the same values that `Interactive::default()` set (`use_punctuation_hint: true`, `punct_extend_ms: 150`, `hold_on_filler: true`, `eou_drain_extended_ms: 1500`, etc.). Cross-check during Task 5 that we are not deleting any default whose value differed from the `HeuristicConfig` mirror.

4. **fono-bench equivalence rows that previously read from
   `cfg.interactive.commit_*` break.**
   Mitigation: Per the audit, the bench harness uses an independent
   `BoundaryConfig` (`crates/fono-bench/src/equivalence.rs:301-330`), not
   `fono_core::config::Interactive`. Confirm during Task 15 with a grep.

5. **CHANGELOG / docs out of sync with the schema.**
   Mitigation: Tasks 16-19 dedicated to docs; Task 17 enforces the
   AGENTS.md release-time changelog rule.

## Alternative Approaches

1. **Deprecate-then-delete across two releases.** Keep the fields with
   `#[deprecated]` attributes for one cycle, emit a tracing warning when
   the user has any of them set in `config.toml`, then delete in the
   following release. Trade-off: smoother user upgrade, but doubles the
   churn and the deprecation pass requires a custom deserializer (serde
   doesn't fire on a successful field parse). Given Fono is pre-1.0 with
   a small user base and unknown keys already deserialize silently, a
   single-step removal is acceptable.

2. **Move the heuristic knobs to a hidden `[interactive.advanced]`
   section instead of deleting them.** Trade-off: preserves
   power-user override capability for the day the boundary heuristics
   are actually plumbed into `LiveSession`, at the cost of leaving a
   nine-field section in the user-visible schema for zero current
   benefit. Rejected because the user-stated goal is *simplification* and
   the override path doesn't exist in the daemon today.

3. **Plumb the heuristic config through `LiveSession::with_heuristics`
   instead of deleting it.** Trade-off: converts inert fields into live
   ones (the audit's recommended remediation), but is a *feature
   addition* not a simplification. Out of scope for this plan;
   appropriate when the boundary heuristics are exercised by real-user
   telemetry and Slice B fixtures justify exposing them as knobs again.
