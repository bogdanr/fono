# Personal vocabulary & voice correction (v3 — simplified)

Supersedes `plans/2026-06-03-correction-with-memory-v2.md`. Re-anchored to
post-v0.14 code and simplified after review: the v2 premise ("correct
`final_text` immediately before injection") was invalidated by v0.10's
word-by-word streaming inject; correcting the STT transcript instead is
both simpler and covers every injection path.

## Objective

Teach Fono once that "Phono" means "Fono" — a user-editable
`vocabulary.toml` deterministically rewrites every future dictation before
the text reaches the cursor, regardless of STT engine and regardless of
whether LLM cleanup is on, off, or streaming.

## Architecture

**Correct the transcript, not the final text.** A pure function
`correction::apply(text, &table) -> String` runs on the raw STT result
immediately after transcription, at exactly two call sites:

- batch: right after hallucination-strip, `crates/fono/src/session.rs:4545`
- live: on stop, `crates/fono/src/session.rs:4134`

Everything downstream — one-shot inject (`:4736`), the v0.10 word-by-word
streaming inject (`stream_cleanup_and_inject`, `:4946`), clipboard
fallback, history, overlay — sees corrected text with no changes to any of
it. The polish LLM receives the canonical spelling and echoes it. As
belt-and-suspenders, the non-streamed `final_text` (`:4723`) gets one more
pass (idempotent, effectively free); the streamed path needs nothing.

**Store:** `~/.config/fono/vocabulary.toml`, resolved via the existing
`Paths` machinery (`crates/fono-core/src/paths.rs:64`), alongside
`config.toml`. Human-editable, dotfile-syncable, never auto-deleted,
outside the drop-on-migration history sqlite.

```toml
[[vocabulary]]
from = ["phono", "phone oh"]   # one or more mishearings (case-insensitive)
to   = "Fono"                  # canonical spelling always emitted
```

**No new config keys.** A missing or empty `vocabulary.toml` is a no-op;
the file's existence is the switch. **No hot-reload plumbing.** The table
is (re)loaded at the start of each dictation — the file is tiny and this
is off the audio hot path — so `fono vocabulary add` is just a file edit
the next dictation picks up.

**Engine invariants** (release gates, all unit-testable, no hardware):
- Whole-word / whole-phrase matching on Unicode word boundaries —
  "phonograph" is never touched. Use `unicode-segmentation` (already in
  `Cargo.lock:4694`; verify with `cargo tree -p fono -i unicode-segmentation`,
  else fall back to `char::is_alphanumeric` boundaries — no new dependency
  either way).
- Multi-word sources, longest-match-first ("phone oh" → "Fono").
- Case-insensitive match via plain Unicode case-fold, canonical-casing
  output. Locale-special cases (Turkish ı, ß) get one documenting sentence
  in the ADR, not bespoke handling.
- Idempotent by construction, guaranteed by two trivial load-time checks
  (no cycle-detection graph analysis needed for a single-pass engine):
  1. no entry's `to` may case-insensitively equal any entry's `from`;
  2. duplicate `from` terms across entries are rejected.
- Malformed file → log a clear error, continue on an empty table; the
  daemon never crashes on user data.

## Implementation Plan

### Phase 1 — Engine & store

- [x] Task 1.1. Short ADR under `docs/decisions/` locking the
      `vocabulary.toml` schema (entry shape, case/boundary semantics in a
      paragraph, file location, never-delete policy). Rationale: the file
      is long-lived and user-authored; the schema is a compatibility
      contract — but one page, not a treatise.
- [x] Task 1.2. `crates/fono-core/src/correction.rs`: `VocabularyEntry`,
      `VocabularyTable`, loader with the two validation checks above, and
      the pure `apply(text, &table) -> String`. Path helper
      `vocabulary_path()` in `paths.rs` next to `config_path()`
      (`crates/fono-core/src/paths.rs:64`); atomic save mirroring
      `Config::save`. Rationale: one module owns store + engine; reuse
      existing XDG/atomic-write infrastructure.
- [x] Task 1.3. Exhaustive unit tests: substring safety
      (phonograph/phonetic/telephone untouched), case variants
      (Phono/phono/PHONO → Fono), multi-word phrase, longest-match-first,
      idempotency (double-apply == single-apply), validation rejections
      (to==from overlap, duplicate from, empty from), Unicode diacritics,
      empty table no-op. Rationale: the pure function is the entire
      correctness guarantee; these are the release gates.

### Phase 2 — Wiring (two lines of integration)

- [x] Task 2.1. Load the table at dictation start and apply to `raw` at
      the two post-STT sites: batch (`crates/fono/src/session.rs:4545`)
      and live (`:4134`); plus the idempotent belt-and-suspenders pass on
      the non-streamed `final_text` (`:4723`). Rationale: upstream-of-
      everything placement is what makes the streaming inject path work
      for free.
- [x] Task 2.2. Integration tests: seeded table produces corrected text
      at the inject boundary in the four-way matrix — {batch, live} ×
      {polish on, polish off} — plus one case exercising the local
      streaming-cleanup path (`stream_cleanup_and_inject` tests around
      `:6019` show the harness pattern). Rationale: the matrix is exactly
      where a missed hook hides; the streaming case is the one v2 would
      have shipped broken.

### Phase 3 — CLI

- [x] Task 3.1. `fono vocabulary add <wrong> <right>` / `remove <wrong>` /
      `list` in `crates/fono/src/cli.rs` — pure file edits, validated
      through the same loader; no IPC verb, no daemon restart needed
      (per-dictation reload). Rationale: zero-friction explicit path;
      minimal plumbing.
- [x] Task 3.2. `fono doctor` line (entry count, file path, parse status)
      and a `docs/configuration.md` section on `vocabulary.toml`.
      Rationale: discoverability and a debugging anchor.

### Phase 4 — Web settings UI (user-requested addition)

- [x] Task 4.1. Vocabulary section in the browser settings page
      (`fono config web`): list entries, add (from/to fields), remove.
      Backed by the same loader/saver, validated server-side with the
      same two checks. Rationale: the settings page is the discoverable
      surface for non-CLI users.
- [x] Task 4.2. Seed the user's vocabulary with the first entry:
      `phono → Fono` (via the shipped tooling, verifying round-trip).

### Deferred (separate slices, unchanged from v2)

- [ ] `fono vocabulary suggest` — mine history raw↔cleaned diffs
      (`HistoryDb::recent()`), y/n confirm, never auto-write. Follow-up PR
      once the core ships.
- [ ] Voice "fix that" hotkey — recall `last_text()`, phonetic match
      (Double-Metaphone + edit distance), show proposal, re-inject,
      auto-record the pair. Own plan when the deterministic engine has
      user mileage.

## Verification Criteria

- `phono → Fono` entry corrects a dictation with polish **off** (pure
  deterministic guarantee).
- Same with polish **on**, including the **local streaming** cleanup path
  where text is typed word-by-word.
- Same in **live/streaming dictation**.
- "phonograph", "phonetic", "telephone" untouched; Phono/phono/PHONO all
  emit "Fono"; double-apply is a no-op.
- Malformed file → clear log line, daemon runs on an empty table.
- `fono vocabulary add/list/remove` round-trips; next dictation picks up
  the change with no daemon restart.
- No new crate outside the existing dependency graph; no new config keys.
- Pre-commit gate green: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --tests --lib`; size budget unchanged
  (`./tests/check.sh --size-budget`).

## Potential Risks and Mitigations

1. **Polish LLM re-introduces the mishearing after seeing corrected input.**
   Mitigation: rare in practice (LLMs preserve proper nouns they are
   given); the non-streamed path gets the free second pass; if field
   reports show it on the streaming path, a WordSink-level pass is the
   contained follow-up — not needed speculatively.
2. **Substring corruption / over-eager replacement.**
   Mitigation: whole-word + longest-phrase-first; substring-safety tests
   are release gates (Task 1.3).
3. **Correcting `raw` changes what history stores.**
   Mitigation: intended — history's `raw` column stores the corrected
   transcript, which is what the user meant; noted in the ADR. The
   deferred `suggest` feature reads raw↔cleaned diffs and is unaffected.
4. **User file lost on crash mid-write.**
   Mitigation: atomic write via the existing `Config::save` pattern;
   stored in the config dir, never inside the history sqlite.

## Alternative Approaches Considered and Rejected

- **v2's post-inject `final_text` pass as the single hook:** broken by the
  v0.10 word-by-word streaming inject (`session.rs:4946`) — the text is
  already at the cursor before any final pass could run. Fixing it there
  needs phrase-holdback buffering in the word streamer; correcting `raw`
  upstream gets the same guarantee with none of that.
- **Whisper `initial_prompt` biasing:** probabilistic, engine-specific,
  cannot guarantee the fix. The polish `dictionary` soft-bias remains a
  separate, complementary UX.
- **`[correction]` config block + daemon hot-reload IPC:** speculative
  machinery; file-presence-as-switch and per-dictation reload deliver the
  same UX with zero new config surface. Either can be added later without
  breaking anything.
- **Cycle detection at load:** a single-pass engine cannot loop; the
  to/from-overlap check is the whole idempotency requirement.
