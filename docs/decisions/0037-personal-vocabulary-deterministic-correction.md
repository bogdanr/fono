# ADR 0037 — Personal vocabulary: deterministic transcript correction

- **Status:** Accepted
- **Date:** 2026-07-03
- **Supersedes:** none
- **Related:** [ADR 0015 — Boundary heuristics](0015-boundary-heuristics.md)
- **Plan:** [`plans/2026-07-03-correction-with-memory-v3.md`](../plans/2026-07-03-correction-with-memory-v3.md)

## Context

Whisper — local and cloud alike — reliably mishears proper nouns,
project names, and jargon ("Phono" for "Fono", "cube ernetes" for
"Kubernetes"). The polish LLM's `[polish.prompt].dictionary` is a soft
bias: inert when polish is off or skipped, probabilistic when it runs.
Users need a guarantee: teach the correction once, get it applied on
every future dictation, deterministically.

## Decision

A user-authored **`vocabulary.toml`** in the config directory
(`~/.config/fono/vocabulary.toml`, resolved through the same XDG
`Paths` machinery as `config.toml`) drives a **pure, deterministic
substitution pass applied to the raw STT transcript immediately after
transcription** — upstream of polish, injection, history, and the
overlay — in both the batch and live dictation paths.

### Why the transcript, not the final text

Since v0.10, local streaming cleanup types the LLM's output into the
cursor **word-by-word as it decodes**; there is no "final text just
before injection" moment on that path. Correcting the transcript means
every downstream consumer — the one-shot inject, the streaming inject,
the clipboard fallback, history, the overlay — sees corrected text with
zero changes to any of them. The polish LLM receives the canonical
spelling and echoes it; the non-streamed batch path applies one extra
idempotent pass to the final text as belt-and-suspenders.

### Schema (compatibility contract)

```toml
[[vocabulary]]
from = ["phono", "phone oh"]   # one or more mishearings (case-insensitive)
to   = "Fono"                  # canonical spelling always emitted
```

- `from` is a non-empty list of non-empty terms; a term may be a
  multi-word phrase.
- `to` is emitted verbatim (canonical casing wins).
- Unknown keys are rejected (`deny_unknown_fields`) so typos surface as
  a clear parse error rather than silently-dead entries.

### Matching semantics

- **Whole-word / whole-phrase only.** The text is tokenized into words
  (maximal runs of Unicode-alphanumeric characters); a rule matches a
  sequence of whole word tokens. "phonograph" is never touched by a
  `phono` rule.
- **Case-insensitive** via plain Unicode case-folding
  (`str::to_lowercase`). Locale-tailored folds (Turkish dotless-ı,
  German ß) are deliberately not special-cased — the default Unicode
  fold is applied uniformly.
- **Multi-word phrases** match across whitespace or hyphens between
  words, never across other punctuation (so "…the phone. Oh, right"
  does not match a `phone oh` rule across the sentence boundary).
- **Longest match first** — a rule matching more words (then more
  characters) wins at any given position.
- **Single pass, idempotent.** Replacements are never rescanned. Two
  load-time checks guarantee `apply(apply(x)) == apply(x)`:
  1. an entry's `to` may not case-insensitively equal another rule's
     `from` term unless both rewrite to the same `to` (this permits
     intentional case-normalization entries like `fono → Fono`);
  2. duplicate `from` terms across entries are rejected.

### Lifecycle

- A missing or empty `vocabulary.toml` is a no-op; the file's existence
  is the feature switch. **No new config keys.**
- The table is (re)loaded at each dictation, off the audio hot path —
  no daemon hot-reload plumbing; `fono vocabulary add` is a plain file
  edit picked up by the next dictation.
- A malformed file logs a clear error and the daemon continues on an
  empty table. User data is **never** auto-deleted or migrated
  destructively; writes are atomic (tempfile + rename), mode 0644.
- The file lives outside the history SQLite (which drops its table on
  schema migration) precisely because vocabulary is long-lived and
  user-authored.
- Consequence of correcting the transcript: history's `raw` column
  stores the *corrected* transcript. This is intended — it is what the
  user meant to say.

## Alternatives considered

- **Final-text pass before injection** (the original v2 plan): broken
  by the v0.10 word-by-word streaming inject; fixing it there requires
  phrase-holdback buffering inside the word streamer.
- **Whisper `initial_prompt` biasing:** probabilistic and
  engine-specific; cannot guarantee the fix. The polish dictionary
  remains as a complementary soft bias.
- **SQLite-backed vocabulary:** not human-editable, not
  dotfile-syncable, and subject to the history DB's drop-on-migration
  policy.
- **Cycle detection at load:** unnecessary — a single-pass engine
  cannot loop; the `to`/`from` overlap check above is the entire
  idempotency requirement.
