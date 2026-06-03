# Personal vocabulary & voice correction

## Objective

Make Fono permanently remember that "Phono" means "Fono" — and any other
word it reliably mishears. The user builds a personal vocabulary once; from
that point on every dictation, regardless of STT engine, regardless of
whether LLM cleanup is on, injects the correct spelling deterministically.
Later, a voice "fix that" command closes the learning loop: one spoken
correction fixes the current text *and* all future dictations.

---

## Architecture

A single integration point: a **deterministic substitution pass** applied to
`final_text` immediately before injection (`crates/fono/src/session.rs:3328-3333`
batch, `:2998-3003` live). No Whisper prompt biasing, no pre-polish rewrite —
Layer C only, because if the pass always runs at the last step the output is
always correct regardless of what happened upstream. Simpler, one place to
test, one place to maintain.

The substitution is backed by a **`vocabulary.toml`** in the config dir —
human-editable, greppable, dotfile-syncable — where every entry maps one or
more misheared forms to the intended canonical spelling.

### Vocabulary store

File: `~/.config/fono/vocabulary.toml` (alongside `config.toml`; path
resolved via the existing `Paths` XDG machinery in
`crates/fono-core/src/paths.rs:32-63`).

Entry shape (finalized in Phase 0 ADR):
```toml
[[vocabulary]]
from = ["phono", "phone oh"]   # one or more mishearings (case-insensitive)
to   = "Fono"                  # canonical spelling always emitted
```

Stored separately from `config.toml` (can grow large; never auto-deleted)
and outside the history SQLite DB (which drops its table on migration,
`crates/fono-core/src/history.rs:81-92`).

### Correction engine (pure)

A new module `crates/fono-core/src/correction.rs` exporting a pure function:

```
fn apply(text: &str, table: &VocabularyTable) -> String
```

Properties that are correctness gates (not aspirational):
- **Whole-word / phrase matching** — word-boundary detection using Unicode
  segmentation. "phonograph" must NOT become "Fonograph".
- **Multi-word sources** — "phone oh" → "Fono" requires phrase-level,
  longest-match-first matching, not single-token only.
- **Case-insensitive match, canonical-casing output** — Phono / phono / PHONO
  all emit "Fono". Unicode case-fold; edge cases (Turkish dotless-i, German ß)
  documented in the ADR.
- **Idempotent** — applying twice equals applying once (both the batch and
  live paths wire the same function; double-application must be safe).
- **Cycle / conflict safety** — rejected at load time; clear error, daemon
  continues on last good vocabulary.
- **No new runtime dependency** — tokenize on Unicode word boundaries,
  hashmap for single tokens + small sliding-window for phrase matching.

### Pipeline wiring

Research confirmed that the **live/streaming path does not call
`run_pipeline`** (`crates/fono/src/session.rs:2882-2891`) — the hook must
be added in **both** `run_pipeline` (batch) and `on_stop_live_dictation`
(live), or streaming dictation silently misses every correction. This is the
principal implementation risk.

### Learning loop

**Phase 1 — explicit CLI:** `fono vocabulary add/remove/list`.

**Phase 2 — suggest:** `fono vocabulary suggest` mines history
`raw`↔`cleaned` pairs (`HistoryDb::last_text()`/`recent()` at
`crates/fono-core/src/history.rs:197-237`) for single-token swaps the user
already accepted via polish, and offers them for one-keystroke confirmation.
Auto-add stays off by default.

**Phase 3 — voice "fix that" (separate slice):** A dedicated correction
hotkey (not an in-dictation trigger phrase — those false-positive badly).
On activation: recall the last injection (`last_text()`), the user speaks
the intended word, Fono finds the closest token by phonetic distance
(Double-Metaphone + edit distance), re-injects the corrected text, and
**auto-records** the (heard → meant) pair into `vocabulary.toml`. This is
the feature that makes the system feel alive; it is a separate slice gated
on the shipped deterministic engine.

---

## Implementation Plan

### Phase 0 — Data model & decisions

- [ ] Task 0.1. Write an ADR under `docs/decisions/` locking the
      `vocabulary.toml` schema: entry fields, `from` multiplicity, canonical
      casing semantics, file location, and migration policy (never delete
      user-authored data, no ALTER on the history sqlite). Rationale: this
      file is long-lived and user-authored — the schema is a compatibility
      contract from day one.
- [ ] Task 0.2. Lock word-boundary and case-fold semantics precisely:
      Unicode word segmentation, canonical-casing-wins on output, documented
      edge cases (Turkish dotless-i, German ß). Rationale: the substring
      safety invariant lives here — "phonograph" must never become
      "Fonograph"; these rules must be pinned before writing the engine.
- [ ] Task 0.3. Define the `[correction]` config block:
      `enabled` (default `true`), paths/defaults. Rationale: every layer
      must be independently disengageable for debugging.

### Phase 1 — Engine & vocabulary store

- [ ] Task 1.1. Add `vocabulary_path()` to `crates/fono-core/src/paths.rs`
      returning `config_dir/vocabulary.toml` (alongside `config.toml` at
      `:61-63`), with load/save helpers mirroring `Config::load`/`save`
      (`crates/fono-core/src/config.rs:1303-1354`) — atomic write, no data
      loss on crash. Rationale: reuse existing XDG + atomic-write
      infrastructure.
- [ ] Task 1.2. Implement vocabulary types (`VocabularyEntry`,
      `VocabularyTable`) and the loader with load-time validation: cycle
      detection, duplicate-`from` conflict rejection, empty/whitespace
      rejection. Bad file → log + fallback to empty table, daemon continues.
      Rationale: fail loudly on a malformed user file without crashing.
- [ ] Task 1.3. Implement `correction::apply(text, &table) -> String`:
      word-boundary + multi-word longest-match-first + canonical-casing +
      idempotent. Rationale: this single pure function is the entire
      correctness guarantee; everything else is wiring.
- [ ] Task 1.4. Exhaustive unit tests for the engine:
      - Substring safety ("phonograph"/"phonetic"/"telephone" untouched)
      - Case variants (Phono/phono/PHONO → Fono)
      - Multi-word phrase ("phone oh" → "Fono")
      - Idempotency (double-apply == single-apply)
      - Longest-match-first (longer phrase wins over shorter prefix)
      - Cycle and conflict rejection at load
      - Unicode diacritics in both source and target
      - Empty table is a no-op
      Rationale: pure + hardware-free means the correctness bar is entirely
      in unit tests; these are release gates.

### Phase 2 — Pipeline wiring

- [ ] Task 2.1. Add the `[correction]` config block to
      `crates/fono-core/src/config.rs` following the `Polish`/`History`
      template (`:491-527`, `:940-952`). Field: `enabled: bool` (default
      `true`). Old configs continue to parse unchanged. Rationale: consistent
      with the established config pattern.
- [ ] Task 2.2. Load the `VocabularyTable` once at daemon startup; thread it
      into **both** `run_pipeline` (batch, `crates/fono/src/session.rs:3088`)
      and `on_stop_live_dictation` (live, `:2882`). Rationale: the live path
      does not call `run_pipeline` — this is the wiring that must not be
      missed; Task 2.4 integration tests will catch it.
- [ ] Task 2.3. Apply `correction::apply` to `final_text` immediately before
      `injector.inject` (batch `:3328-3333`, live `:2998-3003`). Gated on
      `config.correction.enabled`. Rationale: the single integration point;
      this is the only place that must fire; no upstream rewrite needed.
- [ ] Task 2.4. Integration tests: end-to-end dictation with a seeded
      `VocabularyTable` produces the corrected text at the inject call site
      in **both** batch and live paths, with polish enabled and disabled.
      Rationale: the four-way matrix (path × polish-enabled) is exactly where
      a single-hook implementation breaks.

### Phase 3 — CLI learning loop

- [ ] Task 3.1. Add `fono vocabulary add <wrong> <right>`, `remove <wrong>`,
      and `list` subcommands in `crates/fono/src/cli.rs`. Hot-reloads the
      vocabulary in the running daemon. Rationale: the primary explicit path
      to grow the file; zero-friction for the common case.
- [ ] Task 3.2. Add `fono vocabulary suggest`: mine history `raw`↔`cleaned`
      single-token diffs via `HistoryDb::recent()`
      (`crates/fono-core/src/history.rs:227-237`); display candidates with a
      `y/n` prompt; write accepted pairs to `vocabulary.toml`. Auto-add off
      by default. Rationale: turns corrections the user implicitly made via
      polish into permanent deterministic rules, opt-in.
- [ ] Task 3.3. Surface in `fono doctor`: entry count, `correction.enabled`
      state, whether the vocabulary file exists. Document `[correction]` and
      `vocabulary.toml` format in `docs/configuration.md`. Rationale:
      discoverability and a debugging anchor.

### Phase 4 — Voice "fix that" (separate slice)

- [ ] Task 4.1. Add a dedicated correction hotkey / mode (not an
      in-dictation trigger phrase). Rationale: phrase triggers inside normal
      dictation false-positive and corrupt real text.
- [ ] Task 4.2. On activation, recall the last injection (`last_text()`,
      `crates/fono-core/src/history.rs:197-207`), capture the user speaking
      the intended word, find the closest token in the recalled text by
      phonetic distance (Double-Metaphone + edit distance), show the proposed
      swap before committing. Rationale: spoken "Fono" must locate "Phono"
      acoustically; showing the proposal prevents mis-corrections.
- [ ] Task 4.3. Re-inject the corrected text and auto-record the
      (heard → meant) pair into `vocabulary.toml`. Rationale: closes the
      loop — one correction fixes the past and all future dictations.

---

## Verification Criteria

- `vocabulary.toml` entry `phono → Fono` causes "Phono" to be injected as
  "Fono" with **polish disabled** (pure deterministic guarantee).
- Same holds with **polish enabled** — the polished output is corrected
  before injection.
- Holds identically in **live/streaming** dictation, not just batch.
- "phonograph", "phonetic", "telephone" are left untouched.
- Case variants Phono/phono/PHONO all produce "Fono".
- Double-applying the engine is a no-op (idempotent).
- A malformed `vocabulary.toml` (cycle, duplicate, empty `from`) is rejected
  at load with a clear error; the daemon starts on an empty table.
- `fono vocabulary add/list/remove` round-trips correctly.
- `fono vocabulary suggest` proposes history-derived swaps and never
  auto-writes without confirmation.
- Pre-commit gate green: `cargo fmt --check`, `cargo clippy --workspace
  --all-targets -D warnings`, `cargo test --workspace`.

## Potential Risks and Mitigations

1. **Substring corruption / over-eager replacement.**
   Mitigation: whole-word + longest-phrase-first with Unicode word
   boundaries; substring-safety unit tests in Task 1.4 are a release gate.
2. **Live path missed (streaming dictation silently uncorrected).**
   Mitigation: Task 2.2 explicitly hooks both paths; Task 2.4 integration
   tests assert the live path independently.
3. **Case-fold edge cases (Turkish i, German ß).**
   Mitigation: pinned in Task 0.2, documented in the ADR, covered by
   Unicode unit tests.
4. **Cycle / conflicting rules causing unstable output.**
   Mitigation: rejected at load (Task 1.2); engine is single-pass and
   idempotent by construction (Task 1.3).
5. **User file lost on update / accident.**
   Mitigation: atomic write; stored in config dir (not inside the history
   sqlite which drops on migration); `suggest` never auto-writes.
6. **Phase 4 phonetic matcher mis-locating the token to fix.**
   Mitigation: Phase 4 is a separate slice; show the proposed correction
   before committing; the user can decline and type the fix manually.

## Alternative Approaches Considered and Rejected

- **Polish-dictionary-only (existing `[polish.prompt].dictionary`):** Soft
  bias on the LLM, inert when polish is off/skipped. Kept as a separate UX
  for "preferred spellings" but cannot guarantee the fix.
- **Whisper `initial_prompt` biasing + pre-polish rewrite (Layers A & B):**
  Redundant once Layer C (the deterministic post-inject pass) is in place.
  Adding them adds complexity, two more wiring points, and the live/batch
  split risk — for zero additional correctness.
- **SQLite-backed vocabulary:** Queryable but not human-editable; risks the
  history DB's drop-on-migration policy. Rejected for storage; history sqlite
  is still the *source* for `suggest`.
- **Auto-learn every raw→cleaned diff silently:** Pollutes the vocabulary
  with one-off polish rephrasings; risks entrenching bad rules. Replaced by
  opt-in `suggest`.
